# S5 — The Production Flip: Implementation Design (READ-ONLY; pending red-team)

**Crate `libdictenstein`, char ARTrie. 2026-06-03. Design only — NO code edited.** HEAD `b7db8cd`,
on the committed reversible core S0–S4 (all green; S4 formal gate green). This is the FINAL,
IRREVERSIBLE, owner-gated step. Adversarial about the two ways an irreversible flip kills production
data: (1) a global regime mis-applied to mixed Owned/Overlay history; (2) a crash mid-flip leaving a
torn hybrid. Red-team status in §13.

## 0. TL;DR

- **A (multi-segment recovery):** the prompt's premise is partly WRONG, which is good news + exposes a
  different hole. **Production open reads records from the SINGLE active WAL file only**
  (`mmap_ctor::open` `WalReader::new(&wal_path)`) and the regime from THAT file's header → one global
  `reconcile_lww` regime over records that all came from one file ⇒ internally consistent. The
  "d2.6 per-segment-version threading" **does not exist in code**. So the Owned archive is NOT
  mis-dropped by the active Overlay regime — *because normal open never reads the archive*.
- **A2 (the real hole):** the corruption-rebuild paths (`recover_from_archives`,
  `RecoveryManager::rebuild_from_wal`, `rebuild_from_wal_segments`) BYPASS `reconcile_lww` entirely —
  raw segment+LSN order, NO generation ordering, NO regime drop. Post-flip these segments are mixed
  Owned-archive + Overlay-active. **This is the genuine multi-segment data-loss hole.** Latent today
  (opt-in corruption recovery), but S5 makes the format mixed-regime, so S5 MUST make them
  per-segment-regime-aware (recommended, ~30 lines) OR fail-closed on Overlay segments + GAP_LEDGER.
- **B (crash-safety):** flip = owned-`checkpoint()` (folds WAL into data file, leaves active WAL EMPTY)
  → `set_overlay_regime` in-place on the now-empty active (the PROVEN S4 path) → `enable_lockfree` →
  default mode `LockFreeOverlay`. With the checkpoint-first precondition, **no flip-time rotate of
  non-empty data is needed** — it reduces to the S4 empty-WAL stamp.
- **§5 torn window (HIGH):** the on-disk regime flip + the binary's default-mode flip MUST ship in the
  SAME release — else a binary that writes `MAGIC_OVERLAY` but still defaults `OwnedTree` appends OWNED
  (unranked) records to an Overlay WAL → dropped on next reopen → LOST (**ASSUMPTION-4**).
- **D (checkpoint):** the watermark-bounded DESTRUCTIVE reclaim DOES NOT EXIST. S5 ships the
  **retain-WAL** publisher (`publish_immutable_snapshot_retaining_wal`, lossless, unbounded WAL growth)
  and DEFERS destructive compaction — because a naive `rotate_to_archive` archives the `> w` tail that
  normal open never reads ⇒ tail-loss (**ASSUMPTION-8**).
- **Blocker:** `SharedCharARTrie::checkpoint` asserts `lockfree_root.is_none()` (mod.rs:1312) — the flip
  violates it; S5 route-splits the checkpoint body.

## 1. Ground truth (code-read, HEAD b7db8cd)
- **Normal open = single active file, regime-threaded:** `mmap_ctor::open` (302-449) /
  `io_uring_ctor::open_with_io_uring` (126-259) scan ONE file, seed `commit_seq` (S1), load root from
  the data file, read regime from the active header (mmap_ctor.rs:435), feed it to
  `replay_records_lww`→`reconcile_lww`. Archive segments NOT opened here.
- **Corruption rebuild = multi-segment, regime-BLIND:** `rebuild_from_wal_segments` (recovery.rs:1469),
  char `RecoveryManager::rebuild_from_wal` (char recovery.rs:503), `recover_from_archives`
  (mmap_ctor.rs:1167) — apply raw, no regime, no generation order, no drop.
- **5 durable producers** (S4): non-faulting hoist → Order-A append → per-iter `commit_seq` claim →
  rank + `mark_committed`; idempotent NO-RANK + `mark_committed`.
- **`enable_lockfree`** (lockfree_cas.rs:154): builds overlay; stamps Overlay only if
  `writer.current_lsn()==1` (the S4-fix guard `b7db8cd`).
- **`route_overlay()`** = `overlay_write_mode.uses_overlay() && lockfree_root.is_some()` (both required).
  Dormant routing already committed (5a4cc5a) in mutation_api/atomic_ops/batch_insert/document_tx/
  lockfree_value_route. Default `OwnedTree`.
- **Checkpoint:** `publish_durable_and_reclaim` (persist.rs:108) rotates by next_lsn + asserts next_lsn
  unchanged (lock-free-INCOMPATIBLE). `capture_snapshot_immutable` (343, cfg-gated) captures watermark
  BEFORE root load + asserts watermark≤synced_frontier (465). `publish_immutable_snapshot_retaining_wal`
  (548, cfg-gated) RETAINS WAL. **No publisher captures-immutable AND reclaims by watermark.**
- **Header:** `MAGIC`/`MAGIC_OVERLAY` dual-accept (header.rs:128); VERSION=2 already; regime byte 28,
  default Owned, unknown→Owned fail-safe.

## 3. Rotation-at-flip + the multi-segment proof (HEADLINE)
Canonical flip (single-threaded reopen):
```
A. open() normally → owned tree holds all pre-flip data (data file may be stale).
B. checkpoint() [OWNED] → folds WAL into data file (root advanced), rotates spent WAL → archive,
   fresh Owned active is EMPTY (current_lsn()==1).
C. flip_to_overlay(): set_overlay_regime() in-place on the empty active (PROVEN S4 path) →
   enable_lockfree() → set_overlay_write_mode(LockFreeOverlay).
D. (optional) checkpoint() [OVERLAY] to seal.
```
**Proof normal open is safe:** reopen reads recovered_ops from the active (Overlay) file only + the
regime from it; every recovered_op is post-flip Overlay; the Owned archive is never read ⇒ the
Overlay-drop only ever fires on post-flip records. The pre-flip data is in the DATA FILE (via step B's
checkpoint). ∎ **MANDATORY precondition:** checkpoint-before-flip (else pre-flip WAL-only data lands in
the Owned archive that normal open ignores ⇒ LOST — the §3.3 trace).
**A2 fix:** make the rebuild paths reconcile EACH segment with that segment's own header regime
(segments are single-regime by construction); floor-carry across rotate keeps generations globally
monotone. Conservative fallback: fail-closed on any Overlay archive segment + GAP_LEDGER.

## 5. Crash-safety at each flip step (torn-state table)
Crash before A / during B(pre-fsync) / B(post-data-fsync) / after-B-before-C / during-C(set_overlay
mid-fsync): all recover to fully-Owned-old OR fully-Overlay-new (empty-WAL self-heals torn magic). **The
ONE dangerous window:** after C makes `MAGIC_OVERLAY` durable but BEFORE the binary defaults
`LockFreeOverlay`+`enable_lockfree` — owned (unranked) writes to an Overlay file → dropped on reopen →
LOST. **Resolution:** the regime-stamp code and the `LockFreeOverlay` ctor default ship in the SAME
release (true by construction — the stamp is reached only via the flip code that also sets the default).
**ASSUMPTION-4 (HIGH, the #1 red-team item):** verify NO path appends an owned record to a
`MAGIC_OVERLAY` WAL — i.e. every Overlay-stamped V∈{(),u64} trie has `enable_lockfree` run (route_overlay
true) AND no arbitrary-V / value-CAS / doc-tx trie is EVER stamped Overlay.

## 6. Quiesce / lock-ordering
Flip runs at construction on a not-yet-shared `&mut self` ⇒ no concurrent producer/checkpoint/eviction
⇒ nothing to drain. §A hoist stays non-faulting. `set_overlay_regime` lock order = header→file (same as
checkpoint/set_commit_seq_floor). N-S4-3: re-discharge the faulting-producers lock-order via the isolated
insert‖remove‖increment‖checkpoint‖eviction soak.

## 7. Checkpoint-through-overlay (largest NEW unit)
Build `publish_immutable_snapshot_reclaiming` = capture_snapshot_immutable + watermark≤synced_frontier
assert + watermark-bounded reclaim. **HAZARD:** a plain `rotate_to_archive` archives the WHOLE active
file ⇒ records `> w` (the in-flight tail) move to the archive that normal open never reads ⇒ tail-LOSS.
**Correct (7a):** tail-preserving compaction (copy `> w` to a fresh Overlay active, fsync, rename). NEW
~60 lines, highest-risk. **RECOMMENDED for S5 (7b):** ship `publish_immutable_snapshot_retaining_wal`
(retains full WAL, lossless, unbounded growth); defer (7a) to Phase F. Route-split
`SharedCharARTrie::checkpoint` (mod.rs:1298): overlay arm uses capture_immutable + retain; owned arm
keeps the `lockfree_root.is_none()` assert.

## 8. Kill-switch (what it can/cannot undo)
RESTART-time `set_overlay_write_mode(OwnedTree)`. CAN undo WRITES (reopen replays WAL into owned tree).
CANNOT undo the on-disk Overlay format — and worse, owned writes to an Overlay-header WAL get dropped on
reopen. **Resolution: the kill-switch is SYMMETRIC to the flip** — overlay-checkpoint (fold tail) →
rotate → `set_owned_regime()` on a fresh empty active → mode OwnedTree. **ASSUMPTION-6:** S5 must add
`set_owned_regime` (inverse of set_overlay_regime) or the kill-switch is incomplete. Archived Overlay
segments stay Overlay forever (read per-segment-regime by the A2-fixed rebuild). The one-way point =
existence of any Overlay archive segment (old binaries fail-closed).

## 9. Irreversibility boundary
No VERSION bump (additive MAGIC_OVERLAY, dual-accept). One-way point = the first fsync of a
`MAGIC_OVERLAY` header on a production WAL (set_overlay_regime sync_all / rotate fresh-header flush).
Per-file (base/vocab keep MAGIC untouched). Back-compat: new binary reads old Owned WAL unchanged.

## 10. Ordered edit list
1. **S5-1 (A2):** per-segment regime in rebuild_from_wal_segments (recovery.rs:1469), char
   RecoveryManager::rebuild_from_wal (503), recover_from_archives (mmap_ctor.rs:1199). Reversible/inert.
2. **S5-2:** `WalWriter::set_owned_regime()` (inverse, empty-WAL-guarded). Reversible.
3. **S5-3:** un-gate `capture_snapshot_immutable` + `publish_immutable_snapshot_retaining_wal`; promote
   the watermark≤frontier assert to unconditional. Reversible.
4. **S5-4:** `SharedCharARTrie::checkpoint` route-split (mod.rs:1298); move the lockfree_root.is_none()
   assert into the owned arm. Reversible while no ctor flips.
5. **S5-5:** `flip_to_overlay(&mut self)` + symmetric `kill_switch_to_owned(&mut self)`; file-length
   emptiness assert. Reversible (no caller).
6. **S5-6:** multi-segment recovery correspondence test (both polarities), real-disk scratch. Reversible.
7. **S5-7 — THE FLIP (IRREVERSIBLE):** the V∈{(),u64} ctors call `flip_to_overlay()`; first production
   write of MAGIC_OVERLAY. Owner GO + full gate. Arbitrary-V ctors UNCHANGED.

## 11. Gate sequence (irreversible — exhaustive)
cargo check + unsafe-inventory; reconcile_lww unit; NEW per-segment-regime rebuild; NEW multi-segment
correspondence (both polarities); NEW flip-then-crash-at-each-step soak (the §5 torn window +
ASSUMPTION-4); NEW kill-switch round-trip; durable soaks + the N-S4-3 lock-order soak; loom; FULL TLA
(NoLostWriteUnderLockFreeCommit holds + the `_Unsafe` negative controls FAIL); full recovery+char
suites; verify-formal-correspondence.sh. All timeout-wrapped + tee'd, real-disk scratch. **Owner
GO/NO-GO consumed BETWEEN gate-pass and committing S5-7.** S5-1..S5-6 may land before GO (reversible).

## 12. Deferred to Phase F
Delete owned tree; SharedCharARTrie RwLock→Arc; remove route_overlay()==false arms; remove kill-switch;
(7a) tail-preserving destructive compaction; remove capture_snapshot (owned).

## 13. Assumptions for the red-team (all load-bearing)
- **A1:** rotate_to_archive leaves the fresh active at exactly WalHeader::SIZE bytes (soak asserts file
  length, NOT current_lsn which is carried-high).
- **A2:** owned checkpoint() on a recovered-but-stale trie folds 100% of replayed WAL into the data file.
- **A3:** commit_seq_floor carry across rotate makes post-rotate generations strictly exceed pre-rotate
  (per-segment reconcile has no cross-segment generation collision; rebuilt seed = max over segment floors).
- **A4 (HIGH):** NO path appends an owned (unranked) record to a MAGIC_OVERLAY WAL.
- **A5:** capturing the overlay immutable snapshot under the Shared write guard is consistent given
  overlay producers bypass the RwLock (watermark, not lock, provides safety).
- **A6:** the kill-switch needs set_owned_regime + rotate-to-fresh-Owned or it's incomplete.
- **A7:** persist.rs:465 frontier assert is unconditional `assert!` (verify; promote if debug_assert!).
- **A8:** S5 ships retain-WAL (7b) + defers destructive compaction (7a); a naive rotate archives the
  `> w` tail normal open never reads ⇒ loss.

## 14. Red-team findings (2 adversarial passes, 2026-06-03) — VERDICT: DO NOT FLIP AS DESIGNED

Both passes CONFIRM the design's flagged holes AND surface vectors the design did not name. The flip
loses acked production data on the FIRST CLEAN REOPEN after the flip — no crash needed. Ranked:

- **V1 (HIGH — NOT in the design): `open()` never re-establishes the overlay.** Both ctors construct
  with `lockfree_root: None`, `overlay_write_mode: OwnedTree`, and replay into the OWNED tree — they do
  NOT call `enable_lockfree`/set the mode/replay-into-overlay for an Overlay-regime file. So after any
  normal reopen of a flipped file, `route_overlay()==false` ⇒ production `insert`/`remove`/`increment`/
  `commit_document` take the OWNED (unranked) arm and append unranked records onto the Overlay WAL ⇒
  the NEXT reopen's `reconcile_lww(Overlay)` DROPS them all (removed terms resurrect). STEADY-STATE,
  every process, no crash. Fix: `open()` must re-`enable_lockfree` + set `LockFreeOverlay` + REPLAY THE
  TAIL INTO THE OVERLAY for Overlay files (else reads also miss the recovered data).
- **H1/V5 (HIGH — ASSUMPTION-4 FALSE): same-monomorph u64 de-route.** `increment(t, -delta)` (negative)
  and any owned-fallback arm on a FLIPPED u64 trie append an UNRANKED record on the Overlay WAL ⇒
  dropped on reopn. `compare_and_swap`/`commit_document` correctly `Err` under `route_overlay()`, but
  `increment`/`upsert`/the merge drain APPEND. Fix: the owned-fallback arms must REJECT (or emit ranked)
  under `route_overlay()`; `enable_lockfree` must refuse to stamp non-u64 monomorphs.
- **V2 (HIGH): the merge drain** (`merge_lockfree_values_to_persistent`) appends an unranked
  BatchIncrement (both char + byte) ⇒ dropped on an Overlay file. Must be regime-gated/ranked. (S4
  already hit this in `char_lockfree_value_merge`; the fix there was the empty-WAL guard, not this.)
- **H2/A2 (HIGH, conditional): corruption-rebuild is regime-blind** — `rebuild_from_wal_segments`
  (core) AND `RecoveryManager::rebuild_from_wal` (char) bypass `reconcile_lww` (raw LSN order, no drop)
  ⇒ post-flip mixed-regime archives resurrect/double-apply. A3 cross-segment generation collision is
  NOT closed while `commit_seq_floor` is still 0 (DG2 unimplemented). Fix: a SINGLE global reconcile
  pass tagging each record with its segment regime + a populated floor; `publish_snapshot` must also
  write `checkpoint_lsn` to the data header (it doesn't today ⇒ corruption-path double-apply).
- **H3 (MEDIUM-HIGH): the flip emptiness guard is the wrong predicate.** `enable_lockfree`/the flip key
  the Overlay stamp on `current_lsn()==1`, but after the MANDATED checkpoint-first step
  `rotate_to_archive` carries `next_lsn` HIGH (file is empty, length==64, but `current_lsn()` != 1) ⇒
  the stamp is SILENTLY SKIPPED ⇒ an Overlay trie on an OWNED WAL ⇒ NO-RANK orphans KEPT ⇒ resurrection.
  Fix: gate on FILE LENGTH == `WalHeader::SIZE`, not `current_lsn()`; assert `rank_regime()==Overlay`
  after. Same for `set_owned_regime` (kill-switch).
- **H4/A7 (MEDIUM, FALSE): the #41 frontier guard is `debug_assert!`** (persist.rs:464) ⇒ compiled out
  in release. Promote to unconditional `assert!`. (Moot under retain-WAL, load-bearing for any future
  destructive reclaim.) The checkpoint blocker `is_none()` (mod.rs:1312) is likewise debug-only.
- **V4 (MEDIUM): "checkpoint-before-flip" is not enforceable** — emptiness ≠ checkpoint-done. The flip
  must PERFORM the owned checkpoint, not assert WAL-empty.
- **H5/N-S4-3 (MEDIUM stall, not deadlock): `remove`/`increment`/`upsert` fault-in BEFORE the WAL
  append** (buffer lock) — the 75-min-hang CLASS, now production-routed. No lock cycle found (CONFIRM no
  hard deadlock), but a real stall vs a concurrent checkpoint's `buffer.write`. The N-S4-3 soak is
  MANDATORY; consider making remove's pre-flight non-faulting-first like insert.
- **CONFIRMED SOUND:** A5 (overlay capture under the write guard is consistent — safety is the
  watermark-before-root-load ordering, not the lock); the §A insert hoist stays non-faulting; the
  header cannot tear magic-vs-regime (same sector); the retain-WAL publisher is lossless on the NORMAL
  open path; no AB-BA deadlock from S5.

**BOTTOM LINE:** S5 is NOT safe to flip. Required before any irreversible flip: open()-time overlay
re-establishment + replay-into-overlay (V1); regime-gate/reject the owned-fallback u64 producers +
merge drains (H1/V2/V5); per-segment+floor-aware corruption rebuild + data-header checkpoint_lsn (H2/A2);
file-length flip predicate + post-assert (H3); promote the asserts (H4); flip-performs-checkpoint (V4);
the kill-switch `set_owned_regime` symmetry (A6); the N-S4-3 stall soak (H5). MOST of these are
REVERSIBLE hardening (only the final ctor flip, S5-7, is irreversible). Re-red-team after the fixes.
