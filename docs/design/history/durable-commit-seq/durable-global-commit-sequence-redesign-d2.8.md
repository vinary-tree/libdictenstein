# Durable Global Commit-Sequence Redesign — D2.8

**2026-06-02. Read-only design.** D2.8 = D2.7 + the five fixes the D2.7 red-team mandated
(`redteam-dg-recon-findings.md`, "D2.7 RED-TEAM VERDICT"). The (1a) single-LP insert + `CommitSeqMonotone` are
**PROVEN, carried UNCHANGED**. §A idempotent-NO-RANK, §1 `rank_regime` field, §1.4 regime guard, §1.5 regime-keyed
drop, §5 cross-codebase proof — all carried; D8-3 makes §A *precise*, §6 *extends* the cross-codebase proof to the
choke-point + dual-magic. No deferrals.

**Code-verified anchors (this cycle):** char clean-open funnel `mutation_core.rs:252 replay_records_lww` → core
`recovery.rs:253 reconcile_lww` + `mutation_core.rs:286 apply_core_recovered_operation_no_wal`; 3 clean-open callers
`mmap_ctor.rs:403/597`, `io_uring_ctor.rs:227`. RAW-bypass paths: `open_with_recovery_config` inline loop
`mmap_ctor.rs:794-920`; `recover_from_archives:1137`; char `RecoveryManager` `recovery.rs:412/464/503`.
`WalHeader::from_bytes:73` validates magic; `WalReader::new:28` does NOT (seeks past header); only
`read_header:103`+`WalWriter::open:116` reach `from_bytes`. `remove_cas_durable:454` ALREADY hoists a membership
read (`present_before` via `find_leaf_faulting:499`) ABOVE its append `:520` (the §A-hoist template); `insert_cas_durable`
appends at `:329` with only the positive-cache guard `:318`. Base/vocab emit ZERO CommitRank, never call char
`reconcile_lww`/`replay_records_lww`/`apply_core_recovered_operation_no_wal` (grep empty); base calls
`AsyncWalWriter::create` directly (`:121,208`). GREENFIELD (grep empty): `rank_regime`/`RankRegime`/
`ensure_overlay_wal_regime`/`open_with_regime`/`migrate`/`commit_seq`/`commit_seq_floor`.

> **⚠ ORCHESTRATOR ADDENDA (independent scrutiny of D2.8's two self-flagged residuals — §B at end):
> (1) the grep-lint invariant is REQUIRED and must gate CI; (2) the `WalReader::new` clean-scan residual is closed
> on the NEW-binary side by making `WalReader::new` validate the magic set (old binaries remain the irreducible,
> documented mixed-deploy constraint). Folded into §2.3 / §9.**

---

## §1. D8-1 — THE STRUCTURAL FIX: one gated recovery choke-point + the complete gating table

### 1.1 The choke-point
ONE function — the only place any char recovery turns WAL records into owned-tree mutations:
```rust
// persistent_artrie_char/mutation_core.rs (pub(super), generic <V,S>)
fn apply_recovered_records(&mut self, records: Vec<(Lsn,WalRecord)>,
    regime_of: impl Fn(Lsn)->RankRegime, checkpoint_lsn: Lsn, tx_states: &TxStates) -> RecoverApplyOutcome {
    let winners = core::recovery::reconcile_core(records, loaded_from_disk, checkpoint_lsn, &regime_of, tx_states);
    for op in winners { let _ = self.apply_core_recovered_operation_no_wal(op); }   // EXISTING applier (downstream)
    ...
}
```
This GENERALIZES the already-shared `replay_records_lww:252` ("reconcile + apply winners"), which (a) called the
3-arg `reconcile_lww` (no regime/tx) and (b) only 3 callers used. D8-1 makes `replay_records_lww` a thin wrapper over
`apply_recovered_records` and re-points EVERY raw path to it. The public 3-arg `reconcile_lww:253` is retained for its
unit tests as `reconcile_core(.., |_|Owned, &NO_TX)`.

### 1.2 `reconcile_core` — the ONE filtered body (regime-keyed; carried §1.5)
Pass-2 (`recovery.rs:273,280-290`) keys the drop on `regime_of`:
```rust
let cseq = match rank.get(&lsn).copied() {
    Some(s) => s,                                  // ranked → KEEP @ commit_seq
    None => match regime_of(lsn) {
        RankRegime::Overlay => continue,           // Overlay ∧ unranked ⇒ orphan ⇒ DROP
        RankRegime::Owned   => lsn,                // Owned ∧ unranked ⇒ legacy/base/vocab ⇒ KEEP @ lsn (= today's unwrap_or)
    },
};
```
`Owned⇒KEEP@lsn` = byte-for-byte today's `unwrap_or(lsn)` (no regression). R3 ack-after-rank ⇒ acked⟹ranked⟹never
dropped. R6 tx-gating folded into this single body so every caller gets it.

### 1.3 How each entry obtains `regime_of`
Clean-open ×3: records all from the single ACTIVE file ⇒ `regime_of = |_| active_header.rank_regime` (read at the
seed scan `mmap_ctor.rs:292`). Archive/rebuild: records span segments ⇒ build `segment_regime` by reading each
segment's `WalReader::read_header(seg).rank_regime` (`reader.rs:103`); `regime_of(lsn)` resolves via the originating
segment (D2.7 §3.2, now the choke-point's contract).

### 1.4 COMPLETE recovery-path gating table (the structural `NoUngatedV3Recovery` proof)
Grep verb-set: `insert_impl_no_wal`/`insert_impl_no_wal_with_value`/`remove_impl_no_wal`/`try_increment_impl_no_wal`/
`apply_core_recovered_operation_no_wal`/`record_to_operations`/`recovered_operations_from_record`/inline `match record`.

| # | char path | file:lines | today | post-D8-1 | gated |
|---|---|---|---|---|---|
|1|`open` clean replay|`mmap_ctor.rs:403`|`replay_records_lww`|`apply_recovered_records(active-header)`|✅|
|2|`open_with_depth`|`mmap_ctor.rs:597`|"|"|✅|
|3|io_uring clean|`io_uring_ctor.rs:227`|"|"|✅|
|4|`open_with_recovery_config` INLINE loop (P0)|`mmap_ctor.rs:794-920`|RAW `*_impl_no_wal`|**DELETE loop**; collect+per-seg regime; choke-point|✅|
|5|`recover_from_archives`|`mmap_ctor.rs:1137`|`rebuild_from_wal_segments`+RAW apply|collect+per-seg regime; choke-point|✅|
|6|`replay_wal_after_checkpoint` (§7-B5)|char `recovery.rs:464`|RAW `record_to_operations`→`apply_fn`|route via choke-point (active regime)|✅|
|7|`rebuild_from_wal` (torn-tail)|char `recovery.rs:503`|RAW; opens `WalWriter::open:509`|choke-point; **switch `:509`→`WalReader` segment read** (read-only)|✅|
|8|`apply_core_recovered_operation_no_wal`|`mutation_core.rs:286`|applies ONE winner|unchanged — DOWNSTREAM of the gate, only fed by #1-7,9|✅|
|9|`IncrementalRecovery` (core re-export)|core `recovery.rs:894/932`|RAW per-record|per-checkpoint-window reconcile; never-checkpoint-Overlay FAIL-CLOSED→whole-file fallback|✅|
|10|leaf mutators `*_impl_no_wal`|`mutation_core.rs:203/213/224`,`atomic_ops.rs:104`|leaf|NOT recovery entries; recovery callers = #8 only|✅|

**Completeness (structural).** Every record-READING path (#1-7,9) funnels into `apply_recovered_records`→
`reconcile_core` (the ONE gated filter). #8/#10 are leaf appliers fed only by the gate. The grep verb-set is the
COMPLETE set of "record→mutation" verbs in `persistent_artrie_char/`. **A grep-lint CI invariant** ("no leaf-mutator
called inside a `WalReader`/`reader.iter()` loop except via `apply_recovered_records`") makes this machine-checkable
and closes the gap by construction — REQUIRED, not optional (the #9 IncrementalRecovery re-export is the subtle path
that could regrow a gap; gate it at the CORE level: per-window reconcile + fail-closed, which is Owned-safe for base).

### 1.5 base/vocab structurally OUTSIDE the choke-point
base/vocab have their OWN recovery (`RecoveryManager::redo_phase` core `:756`; base inline archive `mmap_ctor.rs:627`
via base's own applier). They NEVER call char `apply_recovered_records`/`replay_records_lww`/char
`apply_core_recovered_operation_no_wal` (grep empty). The choke-point is char-module-`pub(super)`. Unaffected (§6).

---

## §2. D8-2 — DUAL-MAGIC fail-closed tripwire
### 2.1 Constants + magic-set acceptance (ONE place)
```rust
pub const MAGIC: [u8;8]         = *b"PARTWAL\0";   // standard (Owned) — UNCHANGED
pub const MAGIC_OVERLAY: [u8;8] = *b"PARTWALO";    // Overlay-regime files
// from_bytes:73:
let regime = match magic { MAGIC=>Owned, MAGIC_OVERLAY=>Overlay, _=>return Err(CorruptedRecord("Invalid WAL magic")) };
// regime here must AGREE with the byte-28 rank_regime field; mismatch ⇒ corrupt.
```
`from_bytes` is the single magic gate (only `WalWriter::open:116`+`read_header:108` reach it). VERSION stays 2 (no
bump ⇒ F1/F1b stay closed). The widen is ADDITIVE (accepts `MAGIC` exactly as before).
### 2.2 Where the flip writes the overlay magic
`ensure_overlay_wal_regime`'s fresh-file create (`recreate_active_overlay`/`create_overlay`) writes
`WalHeader{magic:MAGIC_OVERLAY, version:2, rank_regime:Overlay,..}`. Owned producers keep `WalWriter::create:87`
writing `MAGIC`. The rotated-away tail keeps `MAGIC` (it was Owned).
### 2.3 Fail-closed + same-binary freedom (+ orchestrator addendum §B)
OLD binary opening an Overlay file via `WalWriter::open`/`read_header` → magic mismatch → Err → FAIL-CLOSED (catches
backup/monitoring/mixed-deploy/rollback readers — a NORMAL-ops vector). NEW binary (recovery/migrate) → `from_bytes`
accepts `MAGIC_OVERLAY` → reads freely (both `read_header` + `open` route through the same `from_bytes`). **Residual:
`WalReader::new:28` seeks past the header (no magic check)**, so an OLD binary's clean-open RECORD SCAN of an Overlay
file is not magic-fail-closed (relies on the byte-28 rank_regime backstop, which an old binary reads as Owned ⇒ keeps
orphans). **§B addendum: in the NEW binary, make `WalReader::new` validate the magic set too** — this closes the
NEW-binary side entirely (new tools using `WalReader::new` on an Overlay file get a clean regime/error). The OLD-binary
`WalReader::new` path is irreducible (can't retro-teach old binaries) and is the documented mixed-deploy constraint —
now strictly smaller than D2.7's (was: every path; now: only an OLD binary's raw `WalReader::new` scan, since the main
ctor opens `WalWriter` first ⇒ magic-checks first). NOTE: the main trie open opens `WalWriter` (magic-checked) BEFORE
the `WalReader::new` scan, so the principal clean-open path IS fail-closed for old binaries; the residual is only a
read-only tool that uses `WalReader::new` without `WalWriter::open`/`read_header`.
### 2.4 Cross-codebase
base/vocab create via `WalWriter::create:87` (`MAGIC`) only; the overlay-magic write is reachable ONLY from
`ensure_overlay_wal_regime` under `route_overlay()` (char-only, grep-confirmed absent in base/vocab). base/vocab keep
`MAGIC` for life; any binary reads them (the standard magic is in the accept set). Unaffected (§6).

---

## §3. D8-3 — §A read-before-append HOIST (resolved: HOIST)
**Decision: HOIST the lock-free membership check above the append** (the shape `remove_cas_durable:498-514` already
proves in production). mark-but-no-rank is the documented contingency, not chosen.
### 3.1 `insert_cas_durable` restructured (`lockfree_cas.rs:291-413`)
Insert the real membership read between `:325` (empty-chars guard) and `:329` (append), mirroring remove's
`present_before`:
```rust
let _epoch = self.epoch_manager.enter_read();
let present_before = self.find_leaf_faulting(lockfree_root,&chars,RETRIES)
    .map(|o| o.map(|n| n.is_final()).unwrap_or(false))
    .unwrap_or_else(|_| self.find_leaf_lockfree(lockfree_root,&chars).map(|n| n.is_final()).unwrap_or(false));
if present_before { lockfree_cache.insert(term.into(), true); return Ok(false); }  // ← appends NOTHING
let lsn = self.append_to_wal_returning_lsn(WalRecord::Insert{..})?;   // :329, now only for a real insert
// CAS loop :344 …
```
The CAS-loop `AlreadyExists` arm `:375` is retained ONLY for the genuine RACE (a concurrent op finalized `t` between
our read and our CAS); per §A it must NOT rank — change `:388` to `mark_committed(lsn)` (watermark liveness) but OMIT
`append_commit_rank` ⇒ its already-appended data record is UNRANKED ⇒ Overlay-dropped. Same for `remove_cas_durable`'s
`AlreadyAbsent` arm `:572` (drop the rank, keep mark). The 4 real producers (`Inserted:367`, `Removed:551`, increments
`:1665/:1763`) keep loop-top-claim+rank.
### 3.2 Linearization soundness (red-team VALIDATED)
`Ok(false)` from `present_before` asserts "`t` present at the read's LP" — not a lost insert (idempotent no-op), exactly
as the existing `remove_cas_durable:498` absent-fast-path. Because the no-op appends NOTHING, there is no commit_seq /
record for it ⇒ the §A resurrection trace is structurally impossible. TOCTOU handled §7-C.

---

## §4. D8-4 — migrate re-ranks legacy CommitRanks
migrate (Owned→Overlay; greenfield) reads a source Owned file that may have LEGACY root-version `CommitRank`s
(cf1f80c-era) whose `generation` would COLLIDE with the fresh Overlay commit_seq space. The re-rank pass: (1) read
source via `WalReader`; (2) `reconcile_core(records, |_|Owned, cp)` → materialized winner set (legacy `CommitRank`
markers consumed in Pass 1, emit no op `recovery.rs:370` ⇒ never leak); (3) write a FRESH Overlay WAL
(`magic=MAGIC_OVERLAY, rank_regime=Overlay`): for each winner append the data record + a FRESH
`CommitRank{data_lsn, generation=next_commit_seq()}` from the new global counter; (4) set the Overlay header's
`commit_seq_floor` to the max migrated commit_seq. Output has ZERO legacy generations; no collision. Source Owned
image+archives stay readable by any binary (`MAGIC`, V2).

---

## §5. D8-5 — flip-primitive crash-mid-flip safety, formalized
```rust
fn ensure_overlay_wal_regime(wal,path,cfg) -> Result<()> {
    if !self.route_overlay() { return Ok(()); }
    match RankRegime::from(WalReader::read_header(path)?) {     // reads any magic
        Overlay => Ok(()),                                     // already flipped ⇒ NO-OP (idempotent)
        Owned => { wal.rotate_to_archive(cfg)?;                // step1: archive Owned tail (MAGIC, floor preserved)
                   wal.recreate_active_overlay(path)?; Ok(()) }// step2: fresh active {MAGIC_OVERLAY, Overlay}
    }
}
```
Hazard: `rotate_to_archive:458` recreates a fresh active via `WalHeader::new()` = `MAGIC`/Owned; a crash between step1
and step2 leaves an Owned-tail active while `route_overlay()` is true. **Crash recovery (D2.7 §1.3 made EXACT):** the
ctor calls `ensure_overlay_wal_regime` BEFORE any overlay producer (char `mmap_ctor.rs:327`); the `match` is the
idempotency — `Owned ∧ route_overlay() ⇒ rotate-again-then-recreate` (double-rotate is safe; the empty Owned segment
is harmless+pruned); `Overlay ⇒ no-op`; mid-rotate ⇒ F5 (rotate is the trusted primitive, rename atomic). The
`open_with_regime(expected)` guard: an overlay producer threads `expected=Overlay`; opening an Owned tail ⇒ Err ⇒
forces the flip. base/vocab/owned thread `expected=Owned` (default `open_or_create_async_wal:490`).
### TLA (extends LockFreeOverlayDurableReplay.tla): actions `CrashMidFlip` (crash between Rotate+Recreate ⇒
active_regime=Owned ∧ route_overlay), `ReopenReRunsFlip`; invariants `FlipConverges` (re-run reaches Overlay),
`NoOverlayProducerOnOwnedTail`; control `_UnsafeNoReRotate` (disable re-run ⇒ fires). Soak
`flip_crash_between_rotate_and_recreate` ≥50× (kill -9 between rotate+recreate; reopen asserts Overlay active + pinned
Owned tail + no resurrection + idempotent reopen).

---

## §6. Carry-over + EXTENDED cross-codebase proof
Carried: (1a)+CommitSeqMonotone+C1′; §A idempotent-NO-RANK (D8-3 precise); §1 rank_regime byte 28 (V2); §1.4 regime
guard; §1.5 regime-keyed drop; §2 reconcile apply-ALL-in-order; R3 ack-after-rank + B#3a increment gen-threading; R6
tx (now in reconcile_core); D4 floor (bytes 20..28; "marked-but-unranked has no commit_seq ⇒ excluded from floor max"
TLA invariant); D5 migrate (§4 re-rank); D6 sentinel; F3 Owned-pin prune; R7 errata.
**EXTENDED proof (choke-point + dual-magic base/vocab-unaffected):** the choke-point is char-`pub(super)`; base/vocab
recover via their own paths, never call it (grep empty). Dual-magic widen is ADDITIVE (`from_bytes` still accepts
`MAGIC`); base/vocab create only `MAGIC` files (the overlay-create is char-only under `route_overlay()`); so no
base/vocab read (forward/rollback) is bricked. Shared touch points all Owned-default/additive (rank_regime byte
Owned-default; magic-set additive; `expected` Owned-default; rotate/truncate Owned→Owned no-op; prune pin only on
mixed; sentinel additive; `reconcile_core` regime param base-never-calls). ∎

---

## §7. SELF-RED-TEAM
**A — choke-point complete?** The grep verb-set finds exactly the #1-#10 sites and nowhere else. Leaf mutators are
also called by route_overlay-false OWNED write paths (not recovery). Residual: a leaf mutator called from a RECOVERY
context other than the choke-point bypasses the gate — D8-1 closes via (a) deleting #4, (b) `pub(super)` + the grep-lint
invariant. The #9 IncrementalRecovery re-export is subtlest (core struct); gate at core level (per-window+fail-closed,
Owned-safe for base). **Complete GIVEN the grep-lint — REQUIRED.**
**B — dual-magic bricks anything?** New binary accepts both → reads freely; base/vocab always `MAGIC` → accepted
(additive). Residual: `WalReader::new` (no magic check) — old binary clean-scan of an Overlay file relies on the
rank_regime backstop. Mitigation §B: new `WalReader::new` validates the magic set (closes new-side); old-side is the
irreducible documented constraint, now smaller (the main ctor opens `WalWriter` magic-checked BEFORE the scan).
**C — §A hoist TOCTOU?** Window exists (read T1, CAS T2). (1) absent→absent: real insert, ranks, correct. (2)
absent→present (raced): CAS `AlreadyExists` → NO-RANK arm → data record dropped, the concurrent insert's rank wins,
correct. (3) present at T1: `Ok(false)` appends nothing; a later remove is the live truth, correct. No idempotent
record ever carries a commit_seq ⇒ the §A resurrection is impossible in ALL interleavings. **TOCTOU benign** (the hoist
ALONE without §A NO-RANK would reintroduce F4 — they are paired).

---

## §8. TLA invariants + `_Unsafe*.cfg`
Carried (D2.7 §8): ReplayEqualsCommittedVisible, NoLostNetWrite, NoResurrectionOnReplay, CommitSeqMonotone,
ReplayEqualsCommittedValue, FloorDominatesSubsumed, SeedAboveDurable, ArchiveNoResurrection, AckImpliesRanked,
NoUncommittedTxReplay, IncrementOutcomeDistinct, OwnedKeepsUnranked, OverlayDropsUnranked, BaseVocabNeverDropped,
OwnedPinSurvivesPrune, IdempotentNoInversion, FlipCreatesFreshOverlay. NEW/sharpened: **`NoUngatedV3Recovery`**
(REDEFINED structural — every recovery action applies ONLY via the single `ApplyRecoveredRecords` choke-point);
**`MarkedUnrankedHasNoCommitSeq`** (floor excludes the §A race-arm lsn); **`FlipConverges`** +
**`NoOverlayProducerOnOwnedTail`**; **`DualMagicFailsClosed`** (Owned-magic reader on an Overlay file ⇒ Err).
**Controls (each MUST fire):** `_UnsafeUngatedRecovery` (REDEF: a recovery action bypasses the choke-point),
`_UnsafeOldBinaryReadsOverlay` (NEW: Owned-magic reader accepts Overlay), `_UnsafeIdempotentRanked` (§A),
`_UnsafeNoReRotate` (D8-5); carried `_UnsafeKeepOverlayOrphan`, `_UnsafeDropOwnedUnranked`, `_UnsafeBaseVocabDropped`,
`_UnsafePruneOwned`, `_UnsafeRegimeMix`, `_UnsafeIncrementSentinel`, `_UnsafeUnrankedIncrement`, `_UnsafeGlobalFloor`,
`_UnsafeNoFloorCarry`, `_UnsafeAckBeforeRank`, `_UnsafeTxIgnored`, `_UnsafeSplitLP`.

---

## §9. DG phases
```
DG0       commit_seq+floor fields+map; rotate/truncate CARRY {floor@20..28, rank_regime@28}     [V2,Owned,reversible]
DG1       (1a) single-LP insert + builder split + increment-rank+gen-thread; §A: insert_cas_durable PRESENT-HOIST
          (mirror remove:498) ⇒ no-op appends nothing; idempotent race arms mark-but-NO-RANK                [reversible]
DG-DECODE D6 IncrementOutcome (no codec)                                                                     [reversible]
DG2       reclaimed-set floor both checkpoint paths; seed-from-floor                                         [reversible]
──────────────── ONE-WAY GATE (DG-RECON) ────────────────
DG-RECON  TOGETHER: (a)+rank_regime byte28; (b) DUAL-MAGIC (MAGIC_OVERLAY+from_bytes accept-set + WalReader::new
          validates set, §B); (c) 4 real producers claim @ loop-top (race arms NO-RANK); (d) D8-1 CHOKE-POINT
          apply_recovered_records+reconcile_core wired into clean-open ×3 + open_with_recovery_config(:794 loop
          DELETED) + recover_from_archives(:1137) + replay_wal_after_checkpoint(:464) + rebuild_from_wal(:503,
          WalReader-based) + IncrementalRecovery(per-window+fail-closed) + GREP-LINT CI invariant; (e) regime guard
          open_with_regime; (f) flip ensure_overlay_wal_regime (recreate writes MAGIC_OVERLAY) + crash-mid-flip
          re-rotate; (g) prune Owned-pin; (h) §7.1 guards
          [Overlay forward-only PER CHAR FILE; migrate sole bridge; VERSION not bumped ⇒ base/vocab/un-flipped-char
           readable by ANY binary forever (MAGIC); Overlay fail-closes OLD binaries via MAGIC_OVERLAY on
           open/rebuild/read_header (+ new-binary WalReader::new)]
DG-MIGRATE migrate (Owned→Overlay) + RE-RANK legacy CommitRanks (D8-4) + CLI                                 [forward]
DG-TX     verify tx-gating in reconcile_core (R6)                                                            [forward]
DG-FORMAL TLA + all controls (NoUngatedV3Recovery structural, DualMagicFailsClosed, FlipConverges,
          MarkedUnrankedHasNoCommitSeq; _UnsafeUngatedRecovery, _UnsafeOldBinaryReadsOverlay,
          _UnsafeIdempotentRanked, _UnsafeNoReRotate)                                                        [HARD GATE]
DG-SOAK   ≥50× real-disk + idempotent-no-op-vs-remove (§A) + flip_crash_between_rotate_and_recreate (D8-5) +
          old-binary-reads-overlay tripwire (D8-2)                                                  [reversible until flip flag]
```
Reversibility: DG0–DG2 code-reversible (fields→0=Owned; VERSION never moves; §A hoist regress-tested vs RB3
`Contains` proptest `:446`). DG-RECON irreversible ONLY per char-Overlay file. No global VERSION bump ⇒ base/vocab +
un-flipped char readable by any binary forever; the one-way boundary is per-file (Overlay-magic).

---

## §B. ORCHESTRATOR ADDENDA (independent scrutiny of D2.8's two residuals)
1. **Grep-lint is REQUIRED + must gate CI.** D2.8 §7-A is right that the choke-point's structural completeness rests
   on "no leaf-mutator (`*_impl_no_wal`/`try_increment_impl_no_wal`/`apply_core_recovered_operation_no_wal`/
   `record_to_operations`) is invoked inside a WAL-record-reading loop except via `apply_recovered_records`." Make this
   a CI grep-lint (a `scripts/lint-recovery-chokepoint.sh` that greps the verb-set against `WalReader`/`reader.iter()`
   contexts), wired into DG-RECON's gate + the formal-correspondence script. Without it the #9 IncrementalRecovery
   path (or a future path) can silently regrow the gap — the lint is the machine-checkable form of `NoUngatedV3Recovery`.
2. **Close the NEW-binary `WalReader::new` residual.** Add a magic-set validation to `WalReader::new` (`reader.rs:22`)
   in the new binary so a new read-only tool scanning an Overlay file gets the regime/Err rather than silently treating
   it as Owned. The OLD-binary `WalReader::new` path is irreducible (can't retro-teach) and is the documented
   mixed-deploy constraint — but it is NARROW (the main ctor opens `WalWriter` magic-checked BEFORE the scan, so the
   principal open path is already fail-closed; only a tool using raw `WalReader::new` on an Overlay file is exposed).
