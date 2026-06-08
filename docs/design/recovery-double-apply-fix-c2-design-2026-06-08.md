# Fix design (C2) for the recovery→checkpoint→reopen counter double-apply (2026-06-08)

> Companion to `recovery-checkpoint-reopen-double-apply-bug-2026-06-08.md` (the confirmed bug).
> **Status: IMPLEMENTED (commit d023074) — full gate green. 2 red-team rounds; the confirming round
> caught a silent-LOSS inversion (use `max_applied_lsn`, NOT `max_lsn_in_segments`) — adopted. 3a is a
> separate pre-existing bug (task #48). One simplification vs the round-1 plan: the `image_coverage_lsn`
> lives in `CommittedWatermark` (its `new()` inits it ⇒ ZERO trie-literal edits) and is read-cleared
> directly in `publish` (no `CheckpointSnapshot` field needed; checkpoints are serialized so "first
> post-recovery checkpoint only" still holds). C2 fixes the bug WITHOUT the L1 generic-V delta arm (it
> makes the reopen SKIP the archive, so the delta arm is never reached).** Source: Plan+red-team passes.

## The keystone insight
`checkpoint_lsn` conflates TWO distinct facts the #41 design happened to unify:
1. The in-memory `committed_watermark` = a **durability** claim ("LSNs 1..=w are durable in THIS
   WAL"). The #41 assert `watermark_at_capture ≤ synced_frontier_at_capture`
   (overlay_checkpoint.rs:295) guards exactly this.
2. The on-disk WAL `Checkpoint.checkpoint_lsn` record = an **image-coverage** fact ("the published
   image already contains the effects of every WAL record with LSN ≤ this"). Drives the reopen
   drain-skip (recovery.rs:318: `loaded_from_disk && checkpoint_lsn>0 && lsn<=checkpoint_lsn → skip`)
   and the prune subsumption (writer.rs:853).

For the recovery path these DIVERGE: the published image DOES contain the archived records' effects
(coverage frontier = `max_lsn_in_archive`), but the durability-of-THIS-WAL frontier is genuinely 0
(the recovered records were applied no-WAL; the fresh WAL never held them).

**Why FIX-C works for `open_inner` but the recovery ctors can't copy it (verified):** `open_inner`
opens the WAL writer OVER the surviving on-disk segments → `AsyncWalWriter::open` →
`set_min_synced_lsn(max)` (async_writer.rs:463) → `synced_frontier == max == watermark`, #41 holds.
The recovery ctors DELETE the corrupt/active WAL (mmap_ctor.rs:908/911) and `create` a FRESH empty
WAL → `synced_frontier=0`. So FIX-C's `mark_committed(max)` seed is ILLEGAL here (panics #41).

## Direction C2 — record the image-coverage frontier WITHOUT inflating the watermark
The recovery ctors stash the rebuild segments' max LSN; the FIRST post-recovery `checkpoint()`
records `checkpoint_lsn = max(watermark, that)` in the WAL `Checkpoint` record ONLY. The in-memory
watermark is NEVER inflated, so the #41 assert (which lives in `capture`, guarding the watermark) is
untouched. The reopen drain-skip then fires for every archived record → applied exactly once.

Rejected alternatives: (A) post-checkpoint archive prune — doesn't help `recover_from_archives`'
foreign archive_dir + new crash-window; keep only as optional belt-and-suspenders. (B1) re-stamp
records into the fresh WAL — violates the no-relog invariant (`active_records==0`). (B2) fake-advance
the synced frontier — IS the #41 footgun (lies that non-durable LSNs are synced). Reject.

## Exact edits (byte; char mirrors). Surface = 3 ctors (byte has NO recover_from_archives).
1. **Struct field** `recovery_image_coverage_lsn: AtomicU64` (default 0) on byte + char structs.
2. **3 recovery ctors** (byte `open_with_recovery_config` ~mmap_ctor.rs:1021 reestablish block; char
   `open_with_recovery_config` ~:1155 + `recover_from_archives` ~:1591): after `reestablish`, store
   `max_lsn_in_segments(&segments_used).unwrap_or(0)` — **`segments_used` (the CONSUMED set), NOT the
   raw enumerated `segments`** (so we never claim coverage of records the image lacks — the rotated/
   unreadable-segment red-team case).
3. **`CheckpointSnapshot`** (overlay_checkpoint.rs:58): add `image_checkpoint_lsn_override: Option<u64>`.
4. **`capture_overlay_snapshot`** (:225): `let cov = self.recovery_image_coverage_lsn.swap(0, AcqRel);
   image_checkpoint_lsn_override: (cov!=0).then_some(cov)`. **The #41 assert (:295) + watermark capture
   (:257) are byte-identical — untouched.** Swap-clear ⇒ only the FIRST post-recovery checkpoint carries
   it (later checkpoints have a real watermark from real durable writes).
5. **`publish_overlay_snapshot_retaining`** (:333) + the `_with_eviction` twin: `checkpoint_lsn =
   base_watermark.max(snapshot.image_checkpoint_lsn_override.unwrap_or(0))`.
6. **COUPLED L1 generic-V delta arm** (flip.rs:1166): genericize to mirror the absolute arm (:1136):
   read current via the i128 `counter_codec`, +delta, `i128_to_counter_value::<V>`, `overlay_publish_value`
   (NOT the u64-only `overlay_publish_counter`). Ship TOGETHER — generic-V alone unmasks i64 to Some(8);
   C2 alone leaves i64 masked. Default override `None`/0 ⇒ steady-state checkpoints byte-identical.

## Red-team round-1 result
CORRECT + #41-safe for the CLEAN path (crash-points 1=mid-recovery, 2=post-recovery-pre-checkpoint,
3b=post-Checkpoint-record, 4=post-checkpoint-pre-reopen) across single/multi/rotated archive layouts,
both V∈{u64,i64}. The #41 assert never fires from the fix (override consumed post-capture). No-relog
preserved (ctors append nothing). Steady-state unaffected (override defaults 0).

**OPEN RISK — crash-point 3a:** crash AFTER the image descriptor fsync but BEFORE the WAL `Checkpoint`
record is durable → reopen sees `loaded_from_disk=true, checkpoint_lsn=Some(0)` → skip FALSE → the
archive RE-DRAINS a DELTA already in the image → double-apply. Agent's claim: this is a PRE-EXISTING
torn-checkpoint window (the same un-checkpointed-delta-tail exists for steady-state overlay checkpoints)
that C2 neither introduces nor closes. **Owner decision pending: (a) accept as the existing contract +
track separately; (b) harden the publish ordering (2-phase "image-not-yet-covered" marker); (c) pair C2
with the optional (A) prune to shrink the window.** This is the subject of the confirming red-team round.

## Regression tests (RED now → GREEN after): add u64 twins to tests/persistent_recovery_watermark_seed_l14.rs
u64 corruption-rebuild + u64 char-archive (RED today = Some(8)); double-reopen idempotence;
`#41-no-panic` lock-in; multi-segment variant. Correct the existing i64 tests' misleading "REFUTED"
comment (they pass via the u64-monomorph masking no-op).

## Confirming red-team (round 2) — CONVERGED on C2 + one BLOCKING correction
**(BLOCKING, caught before impl — a silent-LOSS inversion):** the override must NOT be sourced from
`max_lsn_in_segments(&segments_used)`. That OVER-CLAIMS on interior corruption: (1) `segments_used` is
pushed BEFORE the segment is read (byte mmap_ctor.rs:959), so a mid-segment corrupt record leaves the
partially-applied segment in the set; (2) `max_lsn_in_segments` (writer.rs:591) reads PAST a CRC error
(`WalReader::next_record` advances the cursor by the intact length field on a payload-CRC mismatch,
reader.rs:77-95, so post-corruption records parse + are counted) while the REBUILD stopped at the first
corrupt record (byte mmap_ctor.rs:969 / char :1195 / `rebuild_from_wal_segments_regime_aware`
recovery.rs:1600; also orphan-drop/abort-mid-apply recovery.rs:1647). ⇒ the override could exceed the
last APPLIED lsn ⇒ the reopen drain-skip (`lsn ≤ checkpoint_lsn`) would SKIP the un-applied tail ⇒
**permanent silent LOSS** (worse than the double-apply). **FIX: source the override from
`max_applied_lsn` = the LSN of the last successfully-applied record, tracked INSIDE the rebuild apply
loops (byte inline apply arm; char inline apply arm; the winner-apply in
`rebuild_from_wal_segments_regime_aware`, threaded out via its return). NEVER re-derive from files.**
(`open_inner`'s FIX-C use of `max_lsn_in_segments` is safe only because its segments are full-lifecycle
durable + not behind a break-at-corrupt rebuild — the design copied the helper without its precondition.)

**3a adjudication: (a) PRE-EXISTING, out of scope.** Confirmed with a concrete steady-state trace: a
`<u64>` overlay trie with a durable BatchIncrement delta at LSN N in the live WAL tail, checkpoint folds
it into the image, crash AFTER image-fsync (overlay_checkpoint.rs:341) BEFORE the `Checkpoint` record
fsync (:363) → reopen keeps the PREVIOUS `checkpoint_lsn=P<N` → `drain_segments_into_overlay` re-applies
the delta (flip.rs:1424) on top of the image → double-apply. This fires for plain steady-state u64
checkpoints; C2 does NOT widen it (the torn case leaves the override un-recorded, identical to today).
**Track as a SEPARATE bug (task #48): the retain-WAL two-fsync publisher needs a 2-phase image-coverage
marker.** Do NOT bundle the optional (A) prune as a "fix" — it doesn't cover `recover_from_archives`.

**Everything else verified sound:** #41 assert untouched (override consumed post-capture, never feeds the
in-memory watermark); swap-clear ⇒ only the first post-recovery checkpoint carries it; char parity (2
char ctors); a `checkpoint_lsn` above the active-WAL max is benign for the skip (no records above to
wrongly skip) AS LONG AS it ≤ max-applied (the BLOCKING constraint). **Must also zero-init the new field
in EVERY struct literal/ctor (incl. open_inner :589-627, create) or steady-state leaks the override.**

## Verdict: CONVERGED. Implement C2 by hand with the `max_applied_lsn` correction (NOT max_lsn_in_segments);
## ship the coupled generic-V delta arm together; track 3a as task #48. TDD: u64 RED→GREEN guards.
