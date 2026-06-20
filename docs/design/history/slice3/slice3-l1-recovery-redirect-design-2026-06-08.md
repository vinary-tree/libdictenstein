# Slice 3 / Level 3 — L1 recovery-redirect: converged design + red-team (2026-06-08)

> **Status: REDIRECT BLOCKED on a COUPLED PACKAGE: (1) genericize the overlay counter applier over V, AND
> (2) a #41-aware recovery-checkpoint-reopen watermark fix. Both data-loss-critical, each needs its own
> red-team. The investigation UNCOVERED A CONFIRMED PRODUCTION DATA-LOSS BUG (u64 recovery→checkpoint→reopen
> doubles counter deltas) — see docs/design/recovery-checkpoint-reopen-double-apply-bug-2026-06-08.md.**
>
> **⚠️ CORRECTION — "L1.4 refuted" (below) was ITSELF wrong (a masking artifact).** My first TDD pass found
> the i64 guards GREEN and concluded L1.4 (watermark seed) was unnecessary. WRONG: the green was because the
> u64-only overlay delta applier NO-OPS for i64 (the bug masking the bug). The generic-V applier fix unmasks
> it, AND a `u64` diagnostic CONFIRMED the double-apply is real in production (`Some(8)` for a recovered `+4`).
> So **L1.4 (a watermark fix) IS genuinely required** — but the naive `mark_committed(max_lsn_in_segments)`
> seed violates the #41 capture-ordering invariant (`watermark ≤ synced frontier`; fresh-WAL frontier=0 →
> panic at overlay_checkpoint.rs:295), so the real fix is the deep #41-aware change in the bug doc.
>
> **⛔ REDIRECT BLOCKER (2026-06-08, TDD-caught — the agent's "parity" claim was empirically FALSE):** the
> naive swap `apply_recovered_operation_no_wal` → `apply_recovered_operation_overlay` (L1.1) was implemented
> and **FAILED the committed oracle `byte_corruption_rebuild_replays_batch_increment_and_cas`** (i64
> insert+increment → expected 5, got dropped). Root cause: `overlay_eligible_v()==true` unconditionally
> (overlay_write_mode.rs:487), so a byte `V=i64` trie DOES flip to the overlay, but
> `apply_recovered_operation_overlay`'s Increment-DELTA arm (flip.rs:1166) routes through
> `overlay_publish_counter` / `overlay_counter_get`, which are **hardcoded to the `<u64, S>` monomorph via an
> `Any` downcast** (overlay_write_mode.rs:547/561) and **silently NO-OP for `V=i64`** (and any non-`u64`
> counter). The OLD path (owned `apply_recovered_operation_no_wal` + the STRUCTURAL
> `reestablish_overlay_from_owned`/`build_overlay_root_from_owned`) was generic over V, masking this; the
> redirect's PER-OP overlay counter applier is u64-only. (Normal non-recovery i64 increments work — they use
> the generic `increment_cas` directly; only the RECOVERY/reestablish per-op counter path is u64-specialized.)
> **⇒ PRECONDITION:** make `apply_recovered_operation_overlay`'s Increment-delta (and the absolute arm's
> `overlay_publish_counter` use) GENERIC over V — read the current leaf value generically (0 if absent), add
> the delta via the i128 `counter_codec`, publish via `overlay_publish_value` (generic). This touches the
> i128 counter codec (a DOCUMENTED past BLOCKER — see memory `counter-u64-restoration-done`) AND the SHARED
> applier (the normal-reopen Overlay arm uses it too), so it is **data-loss-critical and needs its own
> red-team** before the redirect lands. **The owned applier `apply_recovered_operation_no_wal` therefore
> CANNOT be deleted at L1.3 yet** — it is the generic-V recovery path until the overlay counter applier is
> genericized (this also touches the G1–G5 overlay-V-genericization track). Reverted commit-clean; guards
> retained (`tests/persistent_recovery_watermark_seed_l14.rs` — they PIN both the no-double-apply AND the
> create-on-missing/i64 properties the redirect must preserve).
>
> **(SUPERSEDED design assumption) L1.4 EMPIRICALLY REFUTED + DROPPED:**
>
> **⚠️ L1.4 REFUTED (2026-06-08, the headline correction):** the confirming-red-team "confirmation" below
> was a CODE-READING FALSE POSITIVE. The TDD regression test (`tests/persistent_recovery_watermark_seed_l14.rs`,
> a BARE BatchIncrement that avoids the SET-masks-delta self-cancel) shows BOTH recovery ctors
> (`open_with_recovery_config` byte + `recover_from_archives` char) apply the delta EXACTLY ONCE across
> recovery→checkpoint→reopen **even with `watermark()==0`**. Root cause the hypothesis missed:
> `open_with_recovery_config` returns the apply-loop trie directly (mmap_ctor.rs:1029 — no re-open, no FIX-C
> seed), but the post-recovery `checkpoint()` captures every recovered record into the image at a HIGHER
> reconcile GENERATION, so the older archived segment loses the LWW reconcile on reopen → no re-drain, no
> double-apply. (FIX-C is needed for the open_inner CONVERSION path because its committed tail is an
> un-checkpointed rotated segment; the recovery path's records are fully checkpoint-subsumed.) **⇒ DROP L1.4**
> (no watermark seed). The GREEN test is retained as the regression guard that the property holds across the
> L1 applier-redirect. Binding conditions reduce to R2 + R1 (below).
>
> **(SUPERSEDED) Confirming red-team (2026-06-08) — claimed L1.4 gap real:** the
> watermark base-seed (`max_lsn_in_segments(archive+active)`) exists ONLY in `open_inner`
> (the normal-reopen path, byte mmap_ctor.rs:486-514 "F7 FIX C"). The corruption/archive-rebuild
> ctor `open_with_recovery_config` (:852-1038) has NO such seed → post-recovery `watermark()==0`
> → the checkpoint→reopen BatchIncrement double-apply hole is REAL (and pre-existing/latent — the
> owned-then-reestablish path also leaves watermark=0). The L1.4 seed
> `max_lsn_in_segments(collect_retained_wal_segments_for_rebuild(...))` spans the FULL drained range
> (collect_retained… renames the active into archive_dir, recovery.rs:1433, so its segment set =
> renamed-active + archives), exactly mirroring the proven FIX-C template at open_inner:507-514.
> ⇒ binding condition #2 (L1.4) is validated; implement L1 by hand.
> Source: Plan-agent design+red-team pass. Carries the L0.2 lesson (verify every
> `route_overlay()` regime assumption). Plan: `slice3-level3-converged-plan-2026-06-08.md`
> L1 section (lines 100-116). Retrospective that motivated the rigor:
> `slice3-l02-rollback-2026-06-08.md`.

## (A) Regime verdict — L1 SOUND, NO flip-hoist needed
The corruption/archive-rebuild ctors install the overlay FIRST, then replay (the INVERSE of
normal reopen). Trace (byte, char identical): `open_with_recovery_config` →
`create(path)` (mmap_ctor.rs:914) → `apply_create_flip` (:172/:276) → `flip_to_overlay`
(flip.rs:542) → `enable_lockfree` sets `lockfree_root=Some(EMPTY)` + stamps Overlay regime —
**THEN** the apply loop runs (:935-988). So `route_overlay()==true` with an empty overlay
installed at the apply loop. `overlay_eligible_v()==true` for ALL V (byte
overlay_write_mode.rs:487, char :131). The redirect target `apply_recovered_operation_overlay`
(flip.rs:1031) early-returns only when `lockfree_root==None` — unreachable here. ⇒ redirecting
the apply closure is regime-correct as-is.

## (B) Converged per-site design — SHAPE = surgical applier-swap (NOT drain-swap)
Keep the existing reconcile/streaming structure (`rebuild_from_wal_segments_regime_aware` for
the Overlay arm; the inline streaming loop for the Owned arm); change ONLY the per-op sink
`apply_*_recovered_operation_no_wal` (owned) → `apply_recovered_operation_overlay` (overlay,
`&self`). Swapping to `drain_segments_into_overlay` (flip.rs:1281) would CHANGE tx-filter
behavior (it adds per-segment RecoveryManager tx-resolution the corruption path does not do
today) — see R4. Do NOT do that.

- **L1.1 byte `open_with_recovery_config`** (src/persistent_artrie/mmap_ctor.rs:852-1038):
  - :938 (Overlay arm) + :977 (Owned inline arm): `apply_recovered_operation_no_wal(op)` →
    `<Self as LockFreeOverlay<ByteKey,V,MmapDiskManager>>::apply_recovered_operation_overlay(&trie, op)`.
  - **DELETE the reestablish block** (:1021-1027) — atomic with the swap (R2).
- **L1.2 char `open_with_recovery_config`** (src/persistent_artrie_char/mmap_ctor.rs:1076-1362):
  - :1160 (Overlay arm) swap as above (CharKey, DiskManager).
  - :1175-1317 (Owned inline arm) is a HAND-ROLLED per-record match over `*_impl_no_wal` —
    REWRITE it to `for op in recovered_operations_from_record(lsn, record) { let _ =
    <Self as LockFreeOverlay<…>>::apply_recovered_operation_overlay(&trie, op); }`
    (`recovered_operations_from_record` is pub, recovery.rs:353). Preserve
    terms_recovered/records_replayed counts. (DRYs byte/char; routes Batch* through the
    shared mapper.)
  - **DELETE the reestablish block** (:1336-1340) — atomic (R2).
- **L1.2 char `recover_from_archives`** (mmap_ctor.rs:1525-1608): :1567 swap; **DELETE
  reestablish** (:1591-1595). (No Owned inline arm — always rebuild_from_wal_segments_regime_aware.)
- **`open_with_full_recovery`** (mmap_ctor.rs:1397): **NO EDIT** — delegates to
  `open_with_recovery_config` (:1445). The plan's :1403 edit-site is a false positive.
- **L1.3 legacy-oracle migration + applier DELETE** (atomic, single commit): migrate
  `open_with_legacy_loader`/`open_with_f5_loader(false)` off owned `replay_records_lww`+reestablish
  (re-point both-loaders/owned-to-overlay suites to production reopen + a BTreeMap oracle, OR
  rewrite the legacy arm to overlay-built correspondence); DELETE the now-dead `open_inner`/io_uring
  LEGACY arms (byte mmap_ctor.rs:675-747 + io_uring_ctor.rs; char mmap_ctor.rs:579-646 +
  open_with_depth:935 + io_uring_ctor.rs:349); DELETE `replay_records_lww` (byte
  mutation_core.rs:493, char :263), `apply_recovered_operation_no_wal` (byte mutation_core.rs:565),
  `apply_core_recovered_operation_no_wal` (char :303), `recompute_recovered_increment` (byte :679),
  `value_from_recovered_i64` (char :410). **KEEP** `*_impl_no_wal` staging mutators (die at L2/L3).
  UNSAFE inventory **0 delta** (none of the 4 deleted fns has an unsafe block; rows 23-24 are in
  the KEPT `*_impl_no_wal` via types.rs). Re-run verify-unsafe-boundary-inventory.sh, expect 0.
- **L1.4 WATERMARK BASE-SEED (REQUIRED — missing from plan; the #41 footgun).** create/
  create_with_config hardcode `CommittedWatermark::new(0)`; overlay no-WAL publishers do NOT
  advance the watermark (flip.rs:323). So post-redirect the trie has a full overlay but
  `watermark()==0` → a subsequent `checkpoint()` records `checkpoint_lsn=0`, the surviving
  archives re-drain on next reopen → BatchIncrement DELTA double-apply (FIX-C class, byte
  mmap_ctor.rs:483-494). **FIX:** after the drain, before return, in each redirected ctor:
  `let max = AsyncWalWriter::max_lsn_in_segments(&segments).unwrap_or(0); if max>0 {
  trie.committed_watermark.mark_committed(max); }` (mark_committed monotone+idempotent,
  committed_watermark.rs:73-90; max_lsn_in_segments pub, async_writer.rs:780). Use the SAME
  `segments` already collected (byte :889 / char :1112 / char-archives :1540). Plausibly also a
  PRE-EXISTING latent bug (owned path also leaves watermark=0).

## (C) Red-team — ranked
- **R2 (most dangerous line):** `reestablish_overlay_from_owned` (flip.rs:946) after redirect
  builds an EMPTY overlay root from the (now-empty) owned tree and `root_ptr.store(empty)`
  (flip.rs:960) → 100% silent loss of the recovered trie. The applier-swap and reestablish-delete
  are ONE atomic unit per ctor — NEVER separate.
- **R1 (process/scope, the L0.2-repeat-risk):** the owned-applier DELETE is sound ONLY because
  `eligible_v()==true` + `USE_F5_REOPEN_LOADER==true` (flip.rs:800) make the LEGACY arm
  unreachable. STALE comments ("ineligible V stays owned", mmap_ctor.rs:580) imply a live path.
  Mitigation: L1.3 commit must cite both ==true facts, migrate the oracle atomically, and add a
  guard/note so a future eligible_v change can't silently resurrect a broken owned applier.
- **R3 (silent, data-corruption):** watermark hole — closed by L1.4. The one axis where
  recover-via-drain ≢ recover-via-owned-then-convert (post-checkpoint-reopen × rotated-archive ×
  u64 BatchIncrement deltas).
- **R4 (parity-on-shape):** tx-filter parity HOLDS for the applier-swap shape (both per-op, no
  tx-filter on the corruption path today); it would BREAK if "upgraded" to drain_segments_into_overlay.
  Op-by-op state parity exact incl. u64>i64::MAX (counter_leaf_to_i128), delta-accumulate, "" root.
- **R5-R8 (low):** prefix-gap guard unchanged; crash-mid-replay converges; multi-segment LWW
  `(generation,lsn)` identical; char inline-match rewrite guarded by
  recovery_replay_completeness_correspondence.rs:241.

## New TLA obligation — RecoveryRebuildOverlay
1. Sink-completeness (lockfree_root≠None at every apply). 2. tx-filter parity (no new filtering —
applier-swap not drain-swap). 3. **Watermark coverage** (∀ visible LSN ℓ: ℓ≤watermark() after the
seed — #41 invariant; the `_Unsafe.cfg` must still exhibit loss when the seed is omitted). 4.
No-relog (0 WAL records appended). + a Rust regression test: corruption-rebuild/recover_from_archives
with a BatchIncrement delta → checkpoint() → drop → reopen → counter NOT double-applied (does not
exist today; guards R3/L1.4).

## (D) Recommendation — SAFE TO IMPLEMENT BY HAND, binding conditions:
1. applier-swap + reestablish-delete atomic per ctor (R2). 2. add L1.4 watermark seed in the same
commit (R3). 3. ONE confirming red-team round AFTER L1.4 + RecoveryRebuildOverlay are written:
validate `max_lsn_in_segments` spans archive+renamed-active (not one segment) for the multi-segment
rotated case, and that the new checkpoint→reopen regression test actually FAILS without L1.4. Keep
route_overlay() computed (untouched). NO unsafe prune at L1 (0 delta).

## L1.4 regression-test construction (a real subtlety — get this RIGHT or the test false-GREENs)
The authoritative bug statement is the FIX-C comment at byte mmap_ctor.rs:483-496: `checkpoint_lsn=0 <
tail_max` ⇒ the archive re-drain skip `tail_lsn <= checkpoint_lsn` is FALSE ⇒ archived records re-drain
⇒ a **BatchIncrement DELTA double-applies**. **Pitfall:** a naive archive of `[BatchInsert(SET 1),
BatchIncrement(+4)]` does NOT expose it on a full (ckpt_lsn=0) re-drain — the re-drained leading SET
resets the counter, MASKING the delta (1+4=5 either way). The genuine repro is the FIX-C scenario: the
committed tail (the delta) lives in an ARCHIVED segment, the checkpoint IMAGE already reflects it, and
the reopen re-drains ONLY that tail on top of the image (no masking SET in the re-drained range) →
image+delta = double. So the regression test must mirror the existing post-conversion/rotated-archive
reconcile tests (study `reconcile_lww_with_regime` + the FIX-C/normal-reopen path), NOT a from-scratch
`[SET,delta]` archive. Verify it is RED on the current recovery ctor (no seed) and GREEN after L1.4.
Cross-check against the open_inner FIX-C path which is already correct (so the test must target
open_with_recovery_config / recover_from_archives specifically).

## Sequencing
L1.1 (byte) and L1.2 (char) independent (commit separately or together) — each fused with its L1.4
seed. L1.3 MUST follow both AND be atomic with the legacy-oracle migration. Per-commit gate: full
suite feature-on + `--no-default-features` + doctests + verify-formal-correspondence.sh exit 0 +
verify-unsafe-boundary-inventory.sh 0-delta + fmt + cross-repo READ-ONLY cargo check (liblevenshtein-rust).
