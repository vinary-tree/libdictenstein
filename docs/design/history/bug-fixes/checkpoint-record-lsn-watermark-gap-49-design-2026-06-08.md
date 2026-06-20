# Fix design (#49) — steady-state checkpoint-record LSN gap stalls the committed watermark → counter-delta double-apply (2026-06-08)

> **Status: IMPLEMENTED + VERIFIED + RED-TEAMED (SAFE-TO-IMPLEMENT, 7/7 attack vectors SOUND).**
> Discovered by #48's regression test `l48_char_u64_double_checkpoint_no_capture_order_panic` (T5), which
> reopened to `Some(14)` instead of `Some(9)`. Distinct root cause from #47 (recovery) and #48 (torn
> record): this one needs **NO corruption** — it fires in normal steady-state operation. Fix landed in the
> 4 retain-WAL overlay publishers (char persist.rs `publish_immutable_snapshot_retaining_wal{,_with_eviction}`;
> byte overlay_checkpoint.rs `publish_overlay_snapshot_retaining{,_with_eviction}`). Regression guards
> RED→GREEN-confirmed (neuter→`Some(14)`/byte `Some(14)`/N-checkpoint `Some(11)=6+5`; restore→`Some(9)`/K).

## Independent red-team verdict (2026-06-08) — SAFE-TO-IMPLEMENT
A dedicated adversarial pass attacked all 7 mandated vectors and could construct no data-loss trace:
1. **No lost write under concurrent writers** — the recorded coverage is FROZEN at capture
   (`checkpoint_lsn = snapshot.committed_watermark_at_capture`); the new `mark_committed(K)` runs in
   Phase C, retroactively inert for the current checkpoint, correcting only the NEXT. K is always above
   any concurrently-committing write's LSN, and contiguity blocks the prefix at any unmarked lower LSN.
2. **#41 preserved** — K is `sync`'d before marked, so `synced_lsn() ≥ K` when the next capture reads
   it ⇒ `watermark ≤ synced_frontier` holds; no mark-before-sync inversion.
3. **Thread-safe** — reuses the existing `Mutex<BTreeSet>`-guarded `mark_committed`; `contiguous` is
   load/stored only under that lock.
4. **Completeness** — `Checkpoint` was the SOLE unmarked-LSN steady-state hole (data/CommitRank are
   marked or burned; `set_commit_seq_floor` consumes NO LSN — it writes the WAL header). Vocab has no
   `CommittedWatermark` (owned-style `checkpoint_lsn = next_lsn-1`); the owned path records `None`
   coverage + truncates. Both correctly excluded.
5. **No on-disk/recovery change** — `mark_committed` is in-memory; reopen reseeds the watermark from the
   recovered frontier. #47/#48 consume the same already-computed `checkpoint_lsn` and inherit the lift.
6. **Owned exclusion safe** — owned and overlay checkpoints are mutually exclusive (route-split
   `assert!(!route_overlay())`); owned truncates + `set_min_lsn`, so no residual owned-Checkpoint LSN
   survives into a later overlay watermark domain.
7. **2-LSN-per-write closes** — data+rank+checkpoint are now ALL marked; the contiguous drain closes the
   run regardless of interleave (the existing `concurrent_committers_converge_to_full_prefix` proof).

**Non-blocking note (PRE-EXISTING, orthogonal — tracked as a separate task):** under char + the
EXPERIMENTAL opt-in `group-commit`, the checkpoint append (`wal_writer.append(Checkpoint)` direct)
bypasses the group-commit coordinator's in-band `allocate_lsn`, so an interleave with the background
`commit_loop`'s strict `append_with_lsn(lsn == current_lsn())` could error a batch (a LIVENESS fault,
not data loss). This is NOT introduced or worsened by #49 (the append predates it; #49 only adds a
`mark_committed` AFTER the existing `append+sync`). Overlay ctors install `group_commit: None`, so the
steady-state overlay path never hits it; byte has no group-commit field. Fix (route the checkpoint
append through the coordinator / take `submit_order`) tracked separately.

**Implementer guardrail (followed):** mark the EXACT `append`-returned LSN — never re-derive from
`synced_lsn()`/`current_lsn()-1` (a concurrent append could make those higher than K, marking a
not-yet-committed neighbor).

## The bug (RED test)
```
create<u64> → durable +4 → checkpoint() → durable +5 → checkpoint()   (live, same instance)
   in-memory value = 9  ✓
drop → reopen → value = 14  ✗   (the +5 delta applied TWICE: image already had 9, and the +5
                                   BatchIncrement re-drained on top → 9 + 5 = 14)
```
`14 = 9 + 5`: the image correctly contains 9 (checkpoint-2 folded the live overlay), but the **+5
BatchIncrement re-drains** on reopen. No torn record, no corruption — a clean reopen loses correctness.

## Root cause (empirically confirmed)
The image-coverage `checkpoint_lsn` recorded by the retain-WAL overlay publisher is
`base_watermark = snapshot.committed_watermark_at_capture` = `CommittedWatermark::watermark()`
(`persist.rs:445`, `:628/:640`). The watermark is the **contiguous committed-LSN prefix**
(`committed_watermark.rs`: a `pending: BTreeSet` whose run-closure advances `contiguous` only over
consecutive marked LSNs).

`mark_committed` is called **only on durable WRITE LSNs** — the Order-A seam
`overlay_write_mode.rs:375` (`mark_committed(data_lsn)`/`mark_committed(rank_lsn)` in
`durable_write.rs:229-230,241`). **`WalRecord::Checkpoint` consumes a WAL LSN (it is a real appended
record, `wal_writer.append` returns its `Lsn`) but is NEVER `mark_committed`'d.** So a checkpoint
record at LSN `K` is a permanent hole in the contiguous prefix:

```
+4   → LSN 1   mark(1)                  contiguous = 1
ckpt1→ LSN 2   (Checkpoint, UNMARKED)   contiguous = 1   ← stuck behind the hole at 2
+5   → LSN 3   mark(3)   pending={3}    contiguous = 1   ← 2 missing ⇒ cannot close to 3
ckpt2 captures watermark = 1  ⇒  records image coverage = 1
reopen: skip WAL records with LSN ≤ 1  ⇒  +5 (LSN 3) is NOT skipped ⇒ RE-DRAINS ⇒ 14
```
(Exact LSNs vary with 1-vs-2-LSN-per-write; the invariant is: the **first** checkpoint record's
unmarked LSN freezes `contiguous`, so **every** later steady-state checkpoint under-claims coverage and
**every** post-first-checkpoint delta re-drains on the next reopen.)

Why existing tests missed it: no test drove live `increment → checkpoint → increment → checkpoint →
reopen` with a value assertion (grep-verified). #48's T7 (repeated torn) PASSES because a **reopen
between** checkpoints reseeds the watermark base to the recovered frontier (covers all LSNs), so the
gap only exists in the **live, no-reopen-between** path. #41's capture assert
(`watermark ≤ synced_frontier`) is also satisfied — the watermark merely *under*-advances, it never
over-advances, so nothing trips; it is a silent over-count, not a panic.

Relationship to #47/#48: same SYMPTOM (post-checkpoint counter delta double-applies on reopen), three
DISTINCT causes — #47 recovery NO-WAL fold (fixed: `max(watermark, max_applied_lsn)`), #48 torn WAL
Checkpoint record (fixed: image self-describes coverage), #49 **the watermark itself under-claims
because checkpoint-record LSNs stall the contiguous prefix**. #48's image-coverage backstop does NOT
fix #49: the image coverage written = the same stalled `checkpoint_lsn` (1), so `max(wal_record=1,
image=1) = 1` — both sources agree on the wrong value.

## Fix — Direction A: mark the Checkpoint record's LSN committed (after its sync)
`wal_writer.append(WalRecord::Checkpoint{..})` already returns the assigned `Lsn` (currently discarded
via `.map_err(..)?`). Capture it; after the `wal_writer.sync()` that makes the record durable, call
`self.committed_watermark.mark_committed(checkpoint_record_lsn)`. The checkpoint record is then
transparent to the contiguous prefix, so the watermark again equals the **committed-write frontier**,
and the next checkpoint's coverage covers every folded delta.

```
+4 → LSN 1 mark(1)                        contiguous = 1
ckpt1 → Checkpoint LSN 2, sync, mark(2)   contiguous = 2   ← hole closed
+5 → LSN 3 mark(3)                        contiguous = 3
ckpt2 captures watermark = 3 ⇒ coverage = 3 ⇒ reopen skips ≤ 3 ⇒ +5 in image, skipped ⇒ 9  ✓
```

**Sites (the 4 retain-WAL OVERLAY publishers only — they record `checkpoint_lsn = watermark` AND
retain the WAL so the gap persists):**
- char `persist.rs:656` `publish_immutable_snapshot_retaining_wal`
- char `persist.rs:784` `publish_immutable_snapshot_retaining_wal_with_eviction`
- byte `overlay_checkpoint.rs:363` (retaining)
- byte `overlay_checkpoint.rs:462` (retaining + eviction)

NOT the owned-path publishers (char `persist.rs:183`, byte `overlay_checkpoint.rs:184`): their snapshot
is `committed_watermark_at_capture = None` (they do not read the watermark for coverage) AND they
`rotate_to_archive` (no retained gap). Marking there would be inert; leave them untouched to minimize
blast radius. Mark order in each site: `append (capture Lsn) → sync → mark_committed(Lsn)`.

## Safety argument (#41 / no-lost-write PRESERVED)
The `LockFreeDurableCheckpoint.tla` `NoLostWriteUnderLockFreeCommit` proof ASSUMES the watermark tracks
the **committed-write** contiguous prefix. The implementation silently violated that assumption by
letting Checkpoint records consume LSNs without marking them (impl-watermark < model-watermark). The
fix **restores the model assumption** (control-record LSNs become transparent), so the existing proof
again applies — it does not weaken it.

- **No write is skipped that is not in the image.** `mark_committed` advances `contiguous` only over a
  contiguous run of marked LSNs. A not-yet-committed write's LSN is unmarked ⇒ blocks the prefix ⇒ the
  watermark can never pass it. A committed write is marked only AFTER its visibility CAS (Order-A) ⇒ it
  is in any snapshot captured after the mark. So everything ≤ watermark is, for writes, in the image;
  for the (now-marked) checkpoint records, vacuous (a control record is nothing to lose). ⇒ skipping
  `≤ coverage` loses no write.
- **`watermark ≤ synced_frontier` (the #41 capture assert) still holds.** The checkpoint record is
  `sync`'d BEFORE it is marked, so its LSN ≤ the synced frontier when marked. The CK lock serializes
  checkpoints; writes do not capture. So the mark cannot push the watermark past the synced frontier.
- **Concurrent writers (lock-free overlay).** Between an overlay capture and the Phase-C checkpoint
  append, a concurrent write at LSN `X` (capture-frontier < X < K) may commit. Marking `K` cannot
  advance `contiguous` past `X` until `X` itself is marked (contiguity). When both are marked, the
  watermark reaches `K`; this affects only the NEXT checkpoint, whose image already includes the
  committed `X`. So coverage ≥ X is correct then (X is in that image). No retroactive effect on the
  current checkpoint (its coverage was already captured pre-append).

## Self-red-team (4 passes — all resolved)
1. **Over-advance past an uncommitted write?** No — contiguity blocks at the unmarked LSN (above).
2. **Mark-before-sync (durability inversion)?** Avoided — mark strictly AFTER `sync()`.
3. **Other unmarked-LSN gap sources?** Steady-state counter path emits BatchIncrement (marked),
   CommitRank (marked, `durable_write.rs:230`), Checkpoint (the only unmarked record). Aborted-write
   gaps don't occur (counters always commit via internal CAS-retry; one append per op). So closing the
   Checkpoint gap closes the only steady-state hole — the T5 regression is the empirical proof.
4. **Recovery domain mismatch?** `append_to_wal_returning_lsn` doc: the returned LSN is "in the
   WAL-writer LSN domain — the same domain `WalRecord::Checkpoint` and recovery use." So the marked LSN
   is the same domain as the watermark and as recovery's skip frontier. Reopen reseeds the watermark
   from the recovered frontier regardless (so the fix is a live-path-only correction; the on-disk
   format is UNCHANGED).

## Regression test (already RED, will GREEN)
`l48_char_u64_double_checkpoint_no_capture_order_panic` (T5): live +4 → checkpoint → +5 → checkpoint →
reopen ⇒ MUST be `Some(9)` (pre-fix `Some(14)`). Plus a byte mirror. Plus an N-checkpoint variant
(+1 each across K checkpoints ⇒ K) to prove the prefix keeps closing, not just once.
