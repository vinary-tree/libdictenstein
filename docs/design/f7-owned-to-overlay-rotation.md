# F7-S4: crash-safe Owned->Overlay conversion-on-reopen (design v1, to be red-teamed)

Unblocks S4 (delete the owned reopen path, no residual): Owned-regime eligible files
(compaction images, kill-switched, legacy) reopen INTO the overlay. F5 cannot convert a
non-empty Owned WAL (install_prebuilt V-2 check + set_overlay_regime rejects non-empty
in-place stamp). Owner chose (A): build the rotation.

## INV-COHERENCE (the crux)
Overlay writes log RANKED (CommitRank) records relying on Overlay orphan-DROP at next
replay. So the active WAL MUST be durably Overlay-regime BEFORE the first overlay write.
"Force-install overlay but keep Owned WAL" is INCOHERENT for a durable file (next reopen
replays overlay-ranked records under Owned KEEP -> orphan resurrection = data loss).

## ARCHITECTURE: eager-on-reopen rotation-conversion
`open_inner`: rank==Overlay -> existing F5 arm; rank==Owned -> NEW
convert_owned_to_overlay_on_reopen (replaces the legacy stay-owned arm). overlay_eligible_v()
is always true post-F2, so there is NO surviving owned-regime reopen arm.
Empty Owned WAL = cheap case: set_overlay_regime (empty OK) + load_root_immutable.

### Conversion sequence (non-empty Owned WAL)
S0. snapshot carried_floor (wal.commit_seq_floor), carried_ckpt_lsn (checkpoint_lsn).
S1. rotate Owned tail -> archive (carries regime+floor into the OLD archived header).
S2. stamp the fresh active WAL Overlay + re-assert floor + FSYNC == DURABLE COMMIT POINT.
S3. load_root_immutable(root_ptr) -> builds + installs the overlay (V-2 now passes).
S4. replay the ARCHIVED Owned tail INTO the overlay via replay_records_lww_overlay(
    rank_regime=Owned) == Owned KEEP semantics (== the old owned reopen's keep-then-LWW).

### The load-bearing DEFECT (must fix)
`AsyncWalWriter::rotate_to_archive` (async_writer.rs:685) does NOT reset the async
`next_lsn` (the sync rotate carries the OLD lsn by DG0 design), so post-rotation
`is_empty_after_header()` is FALSE -> `set_overlay_regime()` REJECTS. Fix: a dedicated
`AsyncWalWriter::rotate_and_restamp_overlay(config)` that, under the writer lock:
rotate_to_archive -> reset inner+async next_lsn/synced_lsn to 1/0 (the fresh segment is
genuinely empty) -> set_overlay_regime -> set_commit_seq_floor(carried_floor). Does NOT
change the shared `rotate_to_archive` contract (checkpoint path needs continue-LSN).

### Crash-safety (durable commit = S2 fsync; conversion is a pure fn of (image, Owned tail))
- before S1 / mid-rename: rename atomic; reopen reads Owned -> re-run. Converges.
- after S1, before S2: active WAL is fresh-Owned (carried regime) + archived Owned tail.
  Reopen reads Owned -> re-run; second rotate archives an EMPTY segment (harmless, pruned);
  the FIRST archive (real tail) is scanned by S4. Converges. (Requires S4 to scan ALL
  archive segments, not just the latest.)
- after S2, before/during S3/S4: active WAL durably Overlay+empty (S3/S4 use NO-WAL
  publishers). Reopen reads Overlay -> F5 arm. **OBLIGATION-A: the F5 arm MUST drain the
  ARCHIVED Owned tail (per-segment regime = Owned KEEP).** Then converges.
- after S4: normal Overlay file. Converges.
Idempotence: durable mutations are (a) additive archive renames (idempotent up to extra
empty segments), (b) Overlay header stamp (idempotent). Replay is LWW over the full set
applied onto the fresh image base -> re-applying yields the same overlay.

### OBLIGATION-A (the #1 red-team item)
The F5-Overlay reopen arm currently replays only the ACTIVE WAL. It MUST also drain
archived segments (collect_wal_segments + per-segment-regime reconcile, like
recover_from_archives) so the post-S2-crash window recovers the archived Owned tail
(KEEP) and any Overlay tails archived under load (DROP). VERIFY whether the normal F5
arm already does this; if not, EXTEND it (promote replay_records_lww_overlay to accept a
per-LSN regime_of, or add a sibling). Most important correctness item.

### Compaction re-point
Switch byte compact() (compaction_impl.rs:331-348) from reopen-Owned-image+reflip to
in-memory reestablish_overlay_from_owned on the empty-WAL post-rename file (clean Overlay
stamp, no rotation). compact() stops producing an on-disk Owned-regime artifact.

### S4 deletions ENABLED (after this + compaction re-point)
Delete: the legacy Owned reopen arms (mmap+io_uring, byte+char), reestablish_overlay_dispatch
+ the 3 per-term folds (reestablish_overlay_membership/_counter/_value). RETAIN (this slice):
clear_owned (still called by the KEPT reestablish_overlay_from_owned used by the
archive-rebuild ctors + compaction) + load_root_from_disk (F5 decode) + the owned_* seam
readers + the owned NODE types/serialize. The owned tree survives ONLY as transient scratch
(load_root_immutable clears it immediately; archive-rebuild ctors materialize-then-convert).
FULL deletion of clear_owned + the owned archive-rebuild appliers = a SUBSEQUENT slice.

### Replay equivalence
replay_records_lww and replay_records_lww_overlay call the SAME reconcile_lww (regime-aware:
recovery.rs:328-334 Owned KEEP @ lsn / Overlay DROP); only the apply target differs. The
F5 both-loaders correspondence already proves the overlay applier reproduces the owned
final state (byte+char × V incl. "" and term-only members). So converted-reopen == old
owned-reopen, incl. unranked Owned entries (orphan-KEEP).

### Verification
- crash-safety proptest (5 crash points × byte/char × V∈{(),u64,String} × empty/non-empty
  WAL/image): inject drop at each step, reopen, assert every committed term+value + "" +
  unranked Owned entries survive + final regime Overlay; idempotence on double reopen.
- TLA+: extend (5-state conversion machine {Owned,Archived,OverlayStamped,OverlayBuilt,
  OverlayReplayed} + crash->reopen), prove every committed record recovered from any crash
  state (validates OBLIGATION-A + double-rotation idempotence). Reuse StorageSyscallOutcome.
- correspondence: converted-reopen snapshot == pre-F7 owned-reopen (open_with_legacy_loader
  test-gated oracle), incl. "" + unranked.
- #41 non-interference: converter never calls checkpoint capture (only WAL header + no-WAL
  publishers + archive replay).

### Rejected alternatives
R1 lazy-on-first-write (read-only reopen sees empty overlay; first-write latency cliff;
spread surface). R2 force-install+rotate-at-checkpoint (violates INV-COHERENCE). R3 in-place
non-empty stamp (formally forbidden — orphan corruption). R4 truncate Owned WAL (loses the
post-checkpoint committed tail). R5 re-log all terms (O(N), breaks LSN/watermark/#41).

## STATUS: design v1 — RED-TEAM before implementing.

# === v2 (red-team #1 resolutions — the coupled LSN BLOCKER + OBLIGATION-A mechanism) ===
Red-team found v1 NEEDS-REVISION: OBLIGATION-A real (all 4 arms active-only); and a COUPLED
BLOCKER pair (#2 LSN-reset breaks global monotonicity recovery.rs:286 -> cross-domain
(generation,lsn) inversion; #3 BatchIncrement delta DOUBLE-APPLY because a post-conversion
checkpoint writes checkpoint_lsn in a NEW low domain so the archive re-drain skip fails).

## v2 FIX A — DO NOT RESET next_lsn (the crux; resolves #2 AND #3 together)
v1's `rotate_and_restamp_overlay` reset next_lsn->1. WRONG. The sync `rotate_to_archive`
ALREADY carries the high next_lsn (DG0) — that IS the global-monotone-LSN invariant
(recovery.rs:286) the LWW sort needs. So:
- KEEP the carried high next_lsn (the fresh active continues the LSN domain; archive LSNs
  are strictly LOWER than all future active LSNs -> no cross-domain inversion). [#2 fixed]
- The DEFECT was only that the Overlay STAMP's emptiness gate uses the next_lsn COUNTER
  (async_writer.rs:604) which is non-1 post-rotate. FIX: gate the stamp on RECORDS-EMPTY =
  file length == WalHeader::SIZE (the fresh active is header-only), NOT counter==1. Add/auth a
  records-empty check (file-length based) and use it in `rotate_and_restamp_overlay` (and have
  `set_overlay_regime` accept a header-only-but-high-LSN active). A header-only WAL has NO
  records -> stamping Overlay is unambiguous (no Owned records to mis-interpret).
- CONSEQUENCE for #3: checkpoint_lsn after conversion is in the SAME continuing domain as the
  archived tail's (lower) LSNs. On re-drain, the skip `tail_lsn <= checkpoint_lsn` is TRUE for
  any tail record a later checkpoint subsumed -> applied EXACTLY ONCE. [#3 fixed]
  Archive pruning after a subsuming checkpoint is a PERF cleanup (correctness rests on the
  lsn-skip, not on removal); reuse the existing prune/segment-lifecycle.

## v2 FIX B — OBLIGATION-A concrete mechanism (all 4 arms + the converter's S4)
The drain of archived segments is a SINGLE shared archive-aware overlay reconcile:
`collect_wal_segments(config)` (writer.rs:594, LSN-ordered) -> `reconcile_lww_with_regime`
(recovery.rs:290) with a per-SEGMENT `regime_of` (each archived segment header carries its
own regime: the converted Owned tail -> KEEP; overlay-written archived tails -> DROP) ->
apply winners via the overlay publishers. Promote `replay_records_lww_overlay` to accept the
per-segment regime closure (or add a sibling `replay_segments_lww_overlay`). WIRE IT INTO ALL
FOUR reopen arms (byte+char × mmap+io_uring) — the Overlay F5 arm AND the new converter's S4
both go through it (the converter is just "Owned active -> rotate -> Overlay file whose tail
is the just-archived segment", so after S2 it IS the Overlay arm draining the archive). This
unifies S4 with the Overlay arm: convert = rotate+stamp+(the shared archive-aware F5 reopen).

## v2 unchanged from v1
The S0-S4 sequence (minus the reset), the crash-safety windows (durable commit = S2 fsync;
now with continuing LSN the post-S2 reopen drains the archive via FIX B), double-rotation
empty-segment (benign; use collect_wal_segments LSN-order), compaction re-point, the
clear_owned residual (honest; archive-rebuild conversion deferred), replay equivalence
(reconcile_lww regime-parametric), verification (crash proptest + TLA 5-state + correspondence
+ #41 non-interference). io_uring twins included (FIX B covers all 4).

## STATUS: v2 — RE-RED-TEAM (the coupled BLOCKER resolution + FIX B).

# === v3 (red-team #2 resolutions — the watermark-seed BLOCKER + FIX A scoping) ===
Round 2: #2 CONFIRMED FIXED (keep high carried next_lsn = DG0 monotonicity). But #3
RESIDUAL: the committed-watermark base at reopen is re-derived by SCANNING the ACTIVE WAL's
records (max_lsn over active records: byte mmap_ctor.rs:499, char :420/:453, io_uring
:181/:212), NOT the writer's next_lsn atom. Post-S2 the active is header-only -> active
max_lsn = 0 -> watermark base = 0 -> first post-conversion checkpoint writes
checkpoint_lsn = 0 < tail_max(12) -> re-drain skip `12 <= 0` FALSE -> BatchIncrement
DOUBLE-APPLIES. The no-WAL drain (apply_recovered_operation_overlay) never mark_committed's.

## v3 FIX C (the #3 fix) — the archive-aware drain MUST advance the watermark
At any archive-aware reopen (the converter S4 AND the Overlay arm draining archived tails),
after applying the drained records, advance `committed_watermark` to cover them:
  CORRECT = SEED the watermark BASE = `max_lsn_in_segments(collect_wal_segments(...))`
  (writer.rs:507) at reopen, instead of active-only. This matches how a NORMAL reopen seeds
  base = active_max (treats ALL records <= max as committed: the image covers <=ckpt_lsn, the
  WAL covers the rest — all durable), just extended to include the ARCHIVE segments.
  REJECTED = `mark_committed(lsn)` per drained tail record: WRONG. The watermark is a
  CONTIGUOUS-PREFIX value; the image-subsumed records (lsn <= checkpoint_lsn=10) are NEVER
  re-applied/marked, so marking only the drained tail (11,12) leaves a GAP at 1..10 -> the
  contiguous prefix stays 0 (watermark never advances). The single-step base-seed sets
  "all <= max committed" DIRECTLY, correct because image+archive+active are ALL durable (the
  archive is the rotated tail = committed Order-A records). Guarantees `watermark() >=
  tail_max` BEFORE the first checkpoint, so checkpoint_lsn >= tail_max, so the re-drain skip
  `tail_lsn <= checkpoint_lsn` is TRUE -> BatchIncrement applied EXACTLY ONCE.
  Trace (fixed): image 5 @ ckpt 10; tail +3 @ lsn 12; convert -> 8, watermark=12; checkpoint
  writes checkpoint_lsn=12; crash+reopen drain skip `12<=12` TRUE -> stays 8. NOT 11.
  NOTE: this watermark-base-from-active-only is a UNIFORM gap (all 4 arms); FIX C corrects it
  for every archive-draining reopen, not just the converter (normal Overlay files keep
  active-only behavior because their archive is already checkpoint-subsumed, so base=max is a
  no-op there; only the un-subsumed converted/under-load archive needs the full-segment seed).

## v3 FIX A scoping (round-2 MAJOR) — the records-empty stamp gate
No file-length emptiness predicate exists; `is_empty_after_header` == `next_lsn==1`
(writer.rs:370, async_writer.rs:604), and writer.rs:367-369 argues against a file-length
check due to BufWriter buffering. Resolution: `rotate_and_restamp_overlay` holds the writer
lock across rotate->stamp and the fresh active is header-only + fsync'd BEFORE the gate
(no buffered records possible in that window), so a file-length records-empty check (len ==
WalHeader::SIZE) is SOUND there. Add `records_empty_on_disk()` (file-length based, used ONLY
on the post-rotate fsync'd active) and gate the Overlay stamp on it (admit a header-only,
HIGH-next_lsn active). Also update the OTHER `current_lsn()==1` emptiness gates that the
converted file hits: flip.rs:430, :439, :460 (the flip/kill-switch regime-stamp guards) —
they must accept the converted (header-only, high-next_lsn) active too, or the converter's
flip path is rejected.

## v3 unchanged: FIX B (archive-aware reconcile via collect_wal_segments +
reconcile_lww_with_regime, all 4 arms + converter S4), the no-reset DG0 monotonicity (#2),
crash windows, compaction re-point, clear_owned residual, replay equivalence, verification
(crash proptest now MUST assert BatchIncrement applied exactly once across the post-S2 crash
+ the watermark>=tail_max invariant; TLA 5-state + the watermark-covers-drain invariant).

## STATUS: v3 — RE-RED-TEAM (FIX C watermark-seed + FIX A scoping).

# === Round-3 verdict: CONVERGED (v3) ===
All 3 BLOCKERs closed, no new BLOCKER (code-verified): #1 FIX B (reconcile_lww_with_regime
+ collect_wal_segments, wired into all 4 arms — mechanism present at recovery.rs:290/1633),
#2 no-reset DG0 monotonicity (rotate carries high next_lsn + floor + regime, writer.rs:571),
#3 FIX C base-seed (CommittedWatermark::new sets contiguous=base directly; AsyncWalWriter::open
ALREADY seeds next_lsn/synced_lsn = max_lsn_in_segments(all)+1 at async_writer.rs:452, so the
capture-ordering assert watermark<=synced_frontier holds). Over-advance to a dropped-orphan
LSN is PRE-EXISTING (active-only base has it today) + benign (orphan ∉ snapshot; no acked
write reclaimed — #41 invariant). FIX A precision NIT: the load-bearing gate is
`set_overlay_regime`'s `is_empty_after_header()==(next_lsn==1)` (writer.rs:383 / async :612),
which `records_empty_on_disk()` targets; the convert path's `install_prebuilt_overlay_root`
(flip.rs:830/835) already admits the high-next_lsn file post-S2-stamp, and flip.rs:430/439/460
+ lockfree_cas enable_lockfree `==1` gates are NOT on the convert path (editing them is
harmless-but-unnecessary). Implementer: target `set_overlay_regime` only.
# === Round-4 double-check pending (the process's "red-team once more") ===

# === v4 (round-4 double-check BLOCKER — crash-loop prune evicts the un-subsumed tail) ===
Round 4 (independent) found a REAL data-loss BLOCKER rounds 1-3 missed: after a crash
AFTER S1 (tail archived) BEFORE S2 (stamp), the fresh active is header-only BUT carries the
high next_lsn (rotate restores next_lsn_after_rotation, writer.rs:582; async never re-syncs).
So `is_empty_after_header()` (next_lsn==1) is FALSE -> the converter MISCLASSIFIES it as a
non-empty Owned WAL -> RE-ROTATES -> mints an empty archive segment each crash-reopen.
`prune_segments_if_needed` (writer.rs:654, oldest-first, max_segments=10) then evicts the
OLDEST = the REAL un-subsumed Owned tail after ~10 crash loops -> the FIX-B drain rebuilds an
INCOMPLETE trie SILENTLY (reconcile_lww_with_regime has NO RES-3 prefix-gap guard). The v1
"second rotate archives an EMPTY segment (harmless, pruned)" claim was WRONG.

## v4 FIX D (the fix) — cheap-vs-ROTATE decision keys on RECORDS-EMPTY-ON-DISK, not next_lsn
The converter decides cheap-vs-rotate via `records_empty_on_disk(active)` (file len ==
WalHeader::SIZE on the fsync'd active), NOT `is_empty_after_header()` (next_lsn==1):
- active RECORDS-EMPTY (header-only — incl. a post-S1-crash high-next_lsn active, AND a
  never-written kill-switched/created Owned file): CHEAP path = stamp Overlay IN PLACE (no
  rotate) -> F5-build from image -> drain ANY EXISTING archive (the prior crash's tail, if
  present) via FIX B. NO new rotate -> NO empty segment minted -> prune never accumulates.
- active RECORDS-NON-EMPTY (a genuine first conversion with un-archived writes): ROTATE the
  tail to archive (S1, exactly ONCE) -> then it becomes records-empty -> stamp -> build ->
  drain. A crash after this S1 lands on the CHEAP path next reopen (drains the archive). So
  at most ONE rotate per conversion; crash-loops never amplify.
This WIDENS FIX A (round-3 "target set_overlay_regime only" was TOO NARROW): the
`records_empty_on_disk` predicate gates BOTH the cheap-vs-rotate decision AND the Overlay
stamp.

## v4 FIX E (defense-in-depth) — RES-3 prefix-gap guard on the FIX-B drain
The FIX-B archive-aware drain must FAIL LOUD (not silently rebuild incomplete) if a committed
prefix is missing: if the min surviving record lsn across (image-frontier, archive, active)
leaves a gap below it that checkpoint_lsn does not cover (min_surviving_lsn > checkpoint_lsn+1),
return a corruption error (the RES-3 guard that today lives ONLY in
rebuild_from_wal_segments_regime_aware recovery.rs:1616 — port it to the FIX-B drain path).
Belt-and-suspenders: exempt un-subsumed segments (first_lsn > checkpoint_lsn) from
`prune_segments_if_needed`. HIGHEST RESIDUAL RISK (implementer): the FIX-B drain must thread
the REAL (loaded_from_disk=true, checkpoint_lsn) into `reconcile_lww_with_regime` — do NOT
reuse `rebuild_from_wal_segments_regime_aware` which hardcodes (false, 0); a wrong
(loaded_from_disk, checkpoint_lsn) reintroduces the FIX-C BatchIncrement double-apply.

## v4 unchanged: FIX B (archive-aware reconcile, all 4 arms), FIX C (watermark base =
max_lsn_in_segments), no-reset DG0 monotonicity, S0-S4 (S1 now conditional on records-non-empty),
compaction re-point, clear_owned residual, replay equivalence, verification (the crash proptest
MUST include the >=11x crash-before-S2 loop asserting the tail is NEVER pruned/lost, + the RES-3
fail-loud on an injected prefix gap). Round-4 confirmed CLEAN: ranked-tail (regime gate
short-circuits recovery.rs:328), empty-image (drain rebuilds from tail), Overlay-arm
subsumed-archive skip, compaction re-point, watermark<=synced_frontier (both seed from
max_lsn_in_segments).

## STATUS: v4 — RE-RED-TEAM (FIX D crash-loop + FIX E RES-3 guard).

# === Round-5 verdict: CONVERGED (v4) — implement, with 2 implementation obligations ===
All 4 BLOCKERs closed (OBLIGATION-A, no-reset monotonicity, FIX-C double-apply, FIX-D
crash-loop). Torn-tail SAFE+BOUNDED (records_empty_on_disk non-empty -> rotate once -> next
reopen cheap; CRC reader stops at the torn prefix, nothing corrupt applied). Prune-exemption
of un-subsumed segments correct (retain un-checkpointed committed data until a checkpoint
subsumes+prunes). Convergence reached across two independent deep passes (r3, r5); the
once-more (r4) caught + fixed the crash-loop. The crash-proptest + TLA + correspondence are
the empirical final gate (test the exact red-team concerns).

## TWO IMPLEMENTATION OBLIGATIONS (must honor — correctness, not design):
- OBL-1 (#1a): `rotate_and_restamp_overlay` MUST `sync_all()` the fresh header after the
  Overlay stamp (the shared `rotate_to_archive` only `flush()`es, writer.rs:579 — file-len is
  accurate for `records_empty_on_disk` regardless, but the EMPTY-Overlay header must be
  fsync-durable across power-cut = the S2 durable commit point).
- OBL-2 (#3, HIGHEST RESIDUAL RISK): the FIX-B drain's `checkpoint_lsn` — used BOTH for the
  RES-3 guard threshold AND the FIX-C `lsn <= checkpoint_lsn` skip — MUST be sourced from the
  LOADED IMAGE/DESCRIPTOR (the dense-image redo frontier), NOT the active-WAL Checkpoint
  record (rotate zeroes the fresh header's checkpoint_lsn, WalHeader::new carries only
  floor+regime, writer.rs:575; RecoveryManager reads the active record = 0 post-rotate). Wrong
  source => RES-3 false-positive on every legitimately-subsumed archive AND FIX-C double-apply.
  Do NOT reuse `rebuild_from_wal_segments_regime_aware` (hardcodes (loaded=false, ckpt=0),
  recovery.rs:1633); thread (loaded_from_disk=true, image_checkpoint_lsn) explicitly.

## STATUS: CONVERGED -> IMPLEMENT (heavily verified: crash proptest + TLA + correspondence).
