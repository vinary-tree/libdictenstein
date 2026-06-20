# CONFIRMED production data-loss bug: recovery → checkpoint → reopen DOUBLE-APPLIES counter deltas (2026-06-08)

> **Status: FIXED (C2) — implemented + full gate green. The fix design + 2-round red-team is in
> `recovery-double-apply-fix-c2-design-2026-06-08.md`; the implementation = `CommittedWatermark`'s
> `image_coverage_lsn` (set from `max_applied_lsn` by the 3 recovery ctors, read-cleared by the first
> post-recovery checkpoint into the on-disk `Checkpoint.checkpoint_lsn`, WITHOUT inflating the watermark).
> u64+i64 RED→GREEN guards in `tests/persistent_recovery_watermark_seed_l14.rs`; 2717-test suite green;
> unsafe-inventory 0-delta. Uncovered during the Slice-3 L1 recovery-redirect investigation; INDEPENDENT of
> (and pre-dated) that work. The related steady-state torn-checkpoint variant is the separate task #48.**

## Symptom (confirmed empirically)
A `PersistentARTrie::<u64>` (the byte counter monomorph — **libgrammstein's n-gram count type**)
that is recovered via `open_with_recovery_config` (corruption rebuild) — or `recover_from_archives`
— then `checkpoint()`'d, then reopened with `open()`, **doubles its recovered BatchIncrement
deltas**. A recovered `+4` reads back as **8** after the reopen (verified: `get_value("counter")
== Some(8)`, expected `Some(4)`).

## Why it was hidden
- The existing recovery oracles (`recovery_replay_completeness_correspondence.rs`) test recovery +
  read, but NOT recovery → **checkpoint → reopen**. So the re-drain on the SECOND open was untested.
- The two `tests/persistent_recovery_watermark_seed_l14.rs` guards use `V=i64`, which the u64-only
  overlay delta applier (`overlay_publish_counter` → `<u64,S>` `Any` downcast, overlay_write_mode.rs:547)
  silently NO-OPS — so for i64 the re-drain is dropped and the counter coincidentally stays correct
  (the bug masks the bug). For `u64` the delta arm works (`increment_cas`), so the double-apply is real.

## Root cause
1. The recovery ctors `open_with_recovery_config` (mmap_ctor.rs:852) and `recover_from_archives`
   return the apply-loop trie directly (mmap_ctor.rs:1029) — they do NOT re-open through `open_inner`,
   so they do NOT inherit the "F7 FIX C" committed-watermark base seed (open_inner mmap_ctor.rs:507).
   Post-recovery `watermark()==0`.
2. `checkpoint()` records `checkpoint_lsn = committed watermark = 0` (overlay_checkpoint.rs:333).
3. `collect_retained_wal_segments_for_rebuild` renamed the recovered WAL into the trie's `wal_archive`
   (a `.segment` file `collect_wal_segments` enumerates), and the checkpoint at lsn=0 subsumes nothing,
   so the archive SURVIVES.
4. The next `open()` → `reconcile_and_drain_overlay` re-enumerates the archive; with
   `image_checkpoint_lsn=0`, every record has `lsn > 0`, so the delta RE-DRAINS on top of the
   checkpoint image (which already includes it) → **double-apply**.

This is the same mechanism "F7 FIX C" fixes for the open_inner Owned→Overlay CONVERSION reopen; it was
never applied to the corruption/archive REBUILD ctors.

## Why the obvious fix is WRONG (and the fix is deep)
The naive seed — `trie.committed_watermark.mark_committed(max_lsn_in_segments(&segments))` after the
drain — **violates the #41 capture-ordering invariant** and panics at overlay_checkpoint.rs:295:
`assert!(watermark_at_capture <= synced_frontier_at_capture)`. The recovery ctor builds a FRESH WAL
(synced frontier = 0; the recovered records were applied no-WAL, never appended to the new WAL), so
seeding `watermark = max_lsn_in_archive (≥1) > 0 = frontier` asserts a committed-but-not-durable LSN.

So the fix must reconcile the watermark with the recovery ctor's fresh-WAL model. Candidate directions
(each needs a #41-aware red-team — this is the most data-loss-critical machinery in the system):
- **(A) Archive-lifecycle:** after the recovery's checkpoint is durable, PRUNE the rebuild archive
  segments so the next reopen has nothing to re-drain (must be crash-safe: prune only post-durable-checkpoint).
- **(B) Frontier reconciliation:** advance the fresh WAL's synced frontier to cover the drained LSNs
  (so `watermark = max ≤ frontier` holds), or re-stamp the recovered records into the new WAL so the
  frontier legitimately reflects them.
- **(C) checkpoint-lsn source:** record `checkpoint_lsn` from the drained-segment max (decoupled from
  the watermark) so the reopen skip works without claiming the watermark is durable — but this splits
  the checkpoint_lsn from the #41 watermark, which the design deliberately unified.

## Interaction with Slice-3 L1
The L1 recovery-redirect ALSO requires genericizing the overlay counter applier over V (the absolute
arm flip.rs:1136 already is; the delta arm flip.rs:1166 must mirror it — read current via
`counter_value_to_i128`, +delta, `i128_to_counter_value`, `overlay_publish_value`). That generic-V fix
is correct + safe (the 283/285 counter/recovery corpus passes) BUT it UNMASKS this double-apply for
i64 too — so the generic-V applier fix and this #41-aware seed are a COUPLED package: neither ships
alone (the generic-V fix without the seed makes i64 reopen worse: 8 instead of the coincidental 4).

## Repro (remove the masking by using u64, or by genericizing the delta arm)
```
let t = PersistentARTrie::<u64>::create(&path)?;            // create-flips to overlay
// replace .wal with [ WalRecord::BatchIncrement{ (b"counter", 4) } ]; corrupt the .part header
let (rec, _) = PersistentARTrie::<u64>::open_with_recovery_config(&path, recovery_config())?;
assert_eq!(rec.get_value("counter"), Some(4));             // recovery OK
rec.checkpoint()?;                                          // checkpoint_lsn = watermark = 0
drop(rec);
let re = PersistentARTrie::<u64>::open(&path)?;
assert_eq!(re.get_value("counter"), Some(4));              // FAILS: Some(8) — DOUBLE-APPLY
```
