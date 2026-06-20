# Durable Global Commit-Sequence Redesign — D2.7

**2026-06-02. Read-only design.** Corrects the regime-encoding + recovery-gating layer of D2.6, closing the four
BLOCKING cross-codebase findings (F1/F1b, F2, F3, F4) from the DG-RECON red-team (`redteam-dg-recon-findings.md`).
**D2's (1a) single-LP insert + `CommitSeqMonotone` (D2 §1.4) + C1′ (D2 §1.7) are PROVEN SOUND, carried UNCHANGED.**
D2.5 R3 ack-after-rank, R5 chokepoint (relocated), R6 tx, R7 errata; D2.6 §1 spine, §2 reconcile-apply-all,
§4 floor, §5 migrate, §6 sentinel, §7.1 char enumeration — all carried UNCHANGED.

**The single break repaired:** D2.6's "regime ≡ WAL VERSION" fails because `WalHeader::VERSION` is GLOBAL (shared
by base `persistent_artrie/mmap_ctor.rs:378`, vocab `persistent_vocab_artrie/mmap_ctor.rs:238`, char — all funnel
through `WalWriter::{open,create}` `writer.rs:101/54`). Bumping it bricks v2 base/vocab files. **D2.7 replaces it
with a dedicated, durable, per-FILE `rank_regime: u8` header field, and does NOT bump VERSION.**

> **⚠ ORCHESTRATOR ADDENDUM (independent code-verified finding — see §A at end): D2.7 §4 below (rank the
> idempotent arm with a loop-top commit_seq) is WRONG — it reintroduces a resurrection. The idempotent arms must
> NOT rank. §A specifies the correction; the next red-team validates the corrected design. The rest of D2.7 stands.**

Key validations gathered this cycle: `reconcile_lww`/`replay_records_lww` is called ONLY by char (`rg` empty in
base+vocab); base recovers via `RecoveryManager::recover` → `redo_phase:756` (raw, in-order, no rank); vocab's
overlay `insert_cas` is a pure in-memory cache (no WAL); vocab/base emit ZERO CommitRank; the shared write
chokepoint is `WalWriter::open`/`create`.

---

## §1. D7-1 — regime is a dedicated, durable, per-FILE `rank_regime: u8` header field (closes F1/F1b)

### 1.1 Field + layout
`WalHeader`: `magic[0..8]`, `version[8..12]`, `checkpoint_lsn[12..20]`, `commit_seq_floor[20..28]` (D4),
**`rank_regime: u8` at byte 28**, `reserved[29..64]`.
```rust
#[repr(u8)] enum RankRegime { Owned = 0, Overlay = 1 }   // default 0 == Owned
```
`WalHeader::new()` sets `rank_regime = Owned`. `to_bytes` writes `buf[28]`; `from_bytes` reads it (unknown byte ⇒
Owned = fail-safe "keep everything"). **VERSION stays 2** (`header.rs:38` UNCHANGED); `version > VERSION` refusal
(`:82`) UNCHANGED — never fires for legit files (none bump past 2). **F1/F1b closed at root:** regime is an
intrinsic per-file reserved-byte property; old readers ignore it (⇒ Owned); no global bump ⇒ no brick; a fresh
base/vocab WAL is `version=2, rank_regime=Owned` and its unranked records are KEPT.

### 1.2 CommitRank.generation disambiguation keys on rank_regime, not VERSION
D2.6 repurposes `CommitRank.generation` (codec type 15, u64 — NO codec change) to carry the durable global
`commit_seq`. The reader decides "commit_seq (Overlay) vs legacy" per-file/per-segment via `rank_regime == Overlay`.
An Owned file's records are all KEPT regardless, so its `rank.unwrap_or(lsn)` sort key is harmless.

### 1.3 The flip sets rank_regime=Overlay on the FRESH char overlay WAL
`ensure_overlay_wal_regime(&wal_writer, path, config)` (D2.6's `ensure_overlay_wal_version`, regime-ized), called
after `open_or_create_async_wal` (char `mmap_ctor.rs:327`) BEFORE any overlay producer: if `route_overlay() ∧
rank_regime==Owned` → `rotate_to_archive` the tail (preserving Owned+floor+version into the archived segment) then
re-create the active file with `rank_regime: Overlay`; if already Overlay → no-op (idempotent across restart,
closes S-A). Owned path never enters this ⇒ its file stays Owned. STRUCTURAL invariant: an Overlay WAL exists only
at/after the flip. (Greenfield: production `enable_lockfree` is `#[cfg(test)]`-only today.)

### 1.4 The R5 guard keys on regime (closes F1/C7)
`WalWriter::open_with_regime(path, expected: RankRegime)`: after `from_bytes`, `found = RankRegime::from_u8(...)`;
`if found != expected { return Err(UnsafeRegimeMixing{found,expected}) }`. `open()` = thin wrapper passing
`Owned` (back-compat). Thread `expected` up through `AsyncWalWriter::{open,create,open_or_create}` (default Owned in
`open_or_create_async_wal:490`); the ONLY caller passing `Overlay` is `ensure_overlay_wal_regime`. Semantics:
Overlay producer opening an Owned file → Err (forces the fresh Overlay WAL); Owned producer ↔ Owned file →
permitted (base/vocab/char-owned, the common case); Owned producer opening an Overlay file → Err (defends a
downgraded binary). `WalReader`/`read_header` UNTOUCHED (read-only, any regime — migration + recovery read freely).
**Strictly safer than D2.6's R5; cannot brick base/vocab** (they are Owned producers on Owned files).

### 1.5 The §2 drop rule keys on rank_regime
Replacing `recovery.rs:286`:
```rust
let cseq = match rank.get(&lsn).copied() {
    Some(s) => s,                              // ranked → KEEP @ commit_seq
    None => match regime_of(lsn) {
        RankRegime::Overlay => continue,       // Overlay ∧ unranked ⇒ orphan ⇒ DROP
        RankRegime::Owned   => lsn,            // Owned ∧ unranked ⇒ legacy/base/vocab ⇒ KEEP @ lsn
    },
};
```
New sig: `reconcile_lww(recovered_ops, loaded_from_disk, checkpoint_lsn, rank_regime: RankRegime, tx_states)`. The
archive path threads a per-record regime via an internal `reconcile_core` taking `regime_of: Fn(Lsn)->RankRegime`
(clean-open: `|_| rank_regime`; archive: per-segment lookup §3.2) — ONE body. Three char `replay_records_lww`
callers (`mmap_ctor.rs:403,597`, `io_uring_ctor.rs:227`) thread `rank_regime` from the active file header (read at
the seed scan `mmap_ctor.rs:292`). `Owned ⇒ KEEP@lsn` = today's `unwrap_or(lsn)` in-order replay (base/vocab/legacy
safe). `Overlay ∧ unranked ⇒ DROP`; R3 ack-after-rank ⇒ `acked ⟹ ranked ⟹ never dropped`.

### 1.6 Validation (the three D7-1 mandates)
(a) base+vocab default Owned, never bricked/dropped: base open `:378`→Owned-guard-permit; base recovery
`redo_phase:756` raw in-order (no reconcile); vocab open `:238`→permit; vocab no rank/reconcile surface. (b) vocab's
no-CommitRank overlay + `merge_lockfree_to_persistent:322` unranked Inserts land in the Owned vocab WAL ⇒ KEEP. (c)
archive per-segment regime reads each segment's `rank_regime` header (§3.2).

---

## §2. D7-2 — recovery-path gating folded INTO the atomic gate (closes F2)
F2 verified: torn-tail `rebuild_from_wal` (char `recovery.rs:503-571`, reached at `:458` on a NORMAL torn-tail
crash) + `IncrementalRecovery` (`core/recovery.rs:959`) + `recover_from_archives`/`rebuild_from_wal_segments` apply
RAW (no reconcile/rank/regime). In an Overlay WAL each resurrects a removed term via an unranked orphan. D2.6
deferred fixing them to DG-PATHS ⇒ a DG-RECON→DG-PATHS crash-window.
**Decision: ALL char recovery paths that can see an Overlay record route through the regime-gated reconcile IN
DG-RECON, not later.**
1. **`rebuild_from_wal` (char `:503`):** union-collect all segments' records, tag each with its segment's
   `rank_regime`, run `reconcile_core` ONCE with `checkpoint_lsn=0`, then apply winners.
2. **`IncrementalRecovery` (core `:856`):** gate per-checkpoint-window through the filtered reconcile; never-
   checkpoint-Overlay (one unbounded window) FAIL-CLOSED → caller falls back to whole-file `replay_records_lww`.
   Shared with base, but base never opens an Overlay file ⇒ for an Owned file it degenerates to KEEP-all-in-order =
   today (no regression).
3. **`recover_from_archives` (char `:1138`) → reconciled sibling** (per-segment regime, cp=0, contiguity §3.2).
   `rebuild_from_wal_segments` is shared with base, so add a `_reconciled` sibling char uses; base's inline archive
   loop (`mmap_ctor.rs:627`, NOT this fn) stays raw/Owned. Base untouched.
**No separate DG-PATHS.** DG-RECON lands the gate complete (clean-open + all recovery paths). `NoUngatedV3Recovery`
(§8) machine-checks completeness.

---

## §3. D7-3 — regime-aware pruning PINS pre-flip Owned archives (closes F3)
F3: `prune_segments_if_needed` (`writer.rs:510`, called every `rotate_to_archive:469`) blindly removes oldest
segments, regime-unaware ⇒ deletes the pre-flip Owned archives the §5 rollback needs.
**Decision: pin Owned segments only when the archive is MIXED (≥1 Overlay segment present).** Read each segment's
`rank_regime` (`reader.rs:103`) when building the prune list (`:518`); partition into `owned`/`overlay`; if mixed,
PIN all `owned` (never prune) and run size/count pruning ONLY over `overlay` (preserving the `remaining<=1` guard
within the overlay set). A pure-Owned archive (base/vocab/pre-flip char) has no boundary ⇒ prunes exactly as today
(no regression). The whole Owned prefix is pinned (not just oldest) ⇒ the rollback chain's contiguity is preserved.

---

## §4. D7-4 — idempotent-arm claim-point + proof (closes F4) — ⚠ SUPERSEDED BY §A (this section is WRONG)
[D2.7 as authored proposed: claim commit_seq at the CAS-retry-loop-top for ALL six arms incl. both idempotent
arms (`:383` AlreadyExists, `:567` AlreadyAbsent), stamping the same loop-top-claimed commit_seq on the idempotent
no-op's CommitRank, with an ordering proof. **The orchestrator found this REINTRODUCES a resurrection — see §A.**
The real producers' loop-top-claim+rank is RETAINED (correct for them); the idempotent arms must NOT rank.]

---

## §5. D7-5 — CROSS-CODEBASE proof: base + vocab UNAFFECTED
**Base (`persistent_artrie/`):** opens `mmap_ctor.rs:121,208,378` + io_uring `:66` — all Owned-default ⇒ guard
permits; appends plain unranked Insert/Remove/Increment/BatchInsert (no `append_commit_rank` — rg empty) to its
Owned file ⇒ KEEP@lsn; recovers `RecoveryManager::recover:645`→`redo_phase:756` raw + inline archive loop
`mmap_ctor.rs:627` — NEVER calls `reconcile_lww` (rg empty). Only shared fn it touches that D2.7 changes is
`recovered_operations_from_record` (D6 sentinel only, additive). **Unaffected.** ∎
**Vocab (`persistent_vocab_artrie/`):** opens `mmap_ctor.rs:238`→Owned; appends unranked Insert/BatchInsert
(`mutation_api.rs:32,49`) + `merge_lockfree_to_persistent:322`→unranked Insert to its Owned WAL ⇒ KEEP; `insert_cas`
(`lockfree_cas.rs:98`) is a pure in-memory cache (NO WAL); ZERO rank/reconcile/CommitRank surface (rg empty).
**The no-Overlay proof:** vocab `enable_lockfree:44-55` sets only `lockfree_root`/`lockfree_cache` — NO fresh WAL,
NO `overlay_write_mode`, NO `route_overlay`/`set_overlay_write_mode`/`ensure_overlay_wal_regime` (rg empty). So
NO vocab path can write `rank_regime=Overlay`. Vocab files are Owned for life. **Unaffected.** ∎
**Shared-surface touch points (only char data-path behavior changes, only when Overlay):** (1) `+rank_regime`
byte 28 (Owned-default); (2) `WalWriter`/`AsyncWalWriter` `+expected: RankRegime` (Owned-default); (3) rotate/
truncate carry regime+floor (Owned→Owned no-op for base/vocab); (4) prune mixed-archive pin (never engages for
pure-Owned); (5) `recovered_operations_from_record` D6 sentinel (additive); (6) `reconcile_lww`/`IncrementalRecovery`/
`rebuild_from_wal_segments` (base doesn't call reconcile_lww; the `_reconciled` sibling leaves raw fns green for base).

---

## §6. Carry-over (UNCHANGED from D2.6/earlier)
(1a) single-LP insert + `CommitSeqMonotone` + C1′; §2 reconcile = apply-ALL-in-(commit_seq,lsn)-order (only the
drop FILTER changes to regime-keyed); R3 ack-after-rank + B#3a increment generation-threading (`try_increment_cas_inner`,
`:1441/:1547`); R6 tx-gating; D4 floor (bytes 20..28, carried across rotate `:458` + truncate `:352`, reclaimed-set
source, map bounded 1<<20 + scan-fallback; floor lives ONLY in Overlay files); D5 migrate (now Owned→Overlay regime
transition, not a VERSION bump — the old Owned image+archives stay readable by any binary; sole forward bridge);
D6 sentinel (`IncrementOutcome{Delta|Absolute(i64)}`, no codec change, DG-DECODE); §7.1 char enumeration +
merge-bridge overlay-reject + `OverlayWalImpliesOverlayLive` asserts; R7 errata.
**REPLACED from D2.6:** regime≡version → dedicated `rank_regime` field (VERSION stays 2); R5 version-guard →
regime-mismatch guard; split DG-RECON→DG-PATHS → recovery-gating folded into DG-RECON; version-unaware prune →
mixed-archive Owned-pin; [unspecified idempotent claim → §A idempotent-NO-RANK].

---

## §7. SELF-RED-TEAM (cross-codebase)
Class A (base/vocab safety): A1 old-writes/new-opens → Owned, permit ✓; A2 new-writes/old-opens → version still 2,
old accepts, ignores byte 28 ✓ (the whole point of NOT bumping); A3 base/vocab never reconcile + Owned-keep ✓; A4
vocab can't set Overlay ✓; A5 rotate/truncate carry Owned (=new() value) + floor ✓; A6 prune pin only on mixed ✓;
A7 corrupt rank_regime byte → Owned/keep fail-safe ✓.
Class B (gate completeness): B1 clean-open gated; B2 torn-tail gated; B3 archive gated; B4 incremental gated+fail-
closed; **B5 — FOUND A SECOND UNGATED PATH: `replay_wal_after_checkpoint` (char `recovery.rs:464`, applies
`record_to_operations` RAW at `:488`, reached on the clean-AutoRecover path) — MUST be gated in DG-RECON too**; B6
base never sees Overlay (guard); B7 migrate reads Owned forward-only; B8 crash-mid-flip safe (idempotent ctor).

---

## §8. TLA `DurableGlobalOrderD27.tla` (extends `LockFreeOverlayDurableReplay.tla`)
Model: `rank_regime: [Files->{Owned,Overlay}]` (immutable per file; flip creates a NEW Overlay file; base/vocab
always Owned); NO global version regime-carrier; `commit_seq: Nat` global fetch_add; drop = `ranked⇒keep@cseq ;
(¬ranked∧Overlay)⇒drop ; (¬ranked∧Owned)⇒keep@lsn`; `floor'=Max{cseq(r):data_lsn≤cp}`; Archive unions
per-segment-regime-tagged segments (cp=0); Prune removes only Overlay when mixed; a `BaseVocab` actor producing
only Owned-unranked records recovering in-order; `IncrementOutcome∈{Delta,Absolute}`.
**Invariants (carried + new):** carried `ReplayEqualsCommittedVisible`, `NoLostNetWrite`, `NoResurrectionOnReplay`,
`CommitSeqMonotone`, `ReplayEqualsCommittedValue`, `FloorDominatesSubsumed`, `SeedAboveDurable`, `NoUnconfirmedWins`,
`ArchiveNoResurrection`, `AckImpliesRanked`, `NoUncommittedTxReplay`, `IncrementOutcomeDistinct`. NEW:
`OwnedKeepsUnranked`, `OverlayDropsUnranked`, `BaseVocabNeverDropped` (F1/F1b headline), `NoUngatedV3Recovery` (F2),
`FlipCreatesFreshOverlay`, `OwnedPinSurvivesPrune` (F3), `IdempotentNoInversion` (F4/§A headline).
**Negative controls (each MUST fire):** `_UnsafeKeepOverlayOrphan`, `_UnsafeDropOwnedUnranked` (F1b base/vocab loss),
`_UnsafeBaseVocabDropped` (F1 — proves the regime field load-bearing), `_UnsafeUngatedRecovery` (F2),
`_UnsafePruneOwned` (F3), `_UnsafeIdempotentRanked` (§A — idempotent ranking ⇒ resurrection), `_UnsafeRegimeMix`;
carried `_UnsafeIncrementSentinel`, `_UnsafeUnrankedIncrement`, `_UnsafeGlobalFloor`, `_UnsafeNoFloorCarry`,
`_UnsafeAckBeforeRank`, `_UnsafeTxIgnored`, `_UnsafeSplitLP`.

---

## §9. DG phases (complete atomic gate; honest reversibility)
```
DG0       commit_seq+floor fields+map; rotate/truncate CARRY {floor@20..28, rank_regime@28}   [V2, Owned-only, reversible]
DG1       (1a) single-LP insert + builder split + increment-rank+gen-thread (8 sites still     [reversible]
          root.version()); idempotent arms NO-RANK (§A)
DG-DECODE D6 IncrementOutcome (no codec)                                                        [reversible]
DG2       reclaimed-set floor both checkpoint paths; seed-from-floor                            [reversible]
──────────────────────  ONE-WAY GATE (DG-RECON) ──────────────────────
DG-RECON  TOGETHER: (a) +rank_regime (plumbed DG0); (b) 4 real producers claim commit_seq @ CAS-loop-top
          (idempotent arms stay NO-RANK §A); (c) regime-gated reconcile wired into clean-open ×3 +
          rebuild_from_wal(:503) + replay_wal_after_checkpoint(:464, §7-B5) + recover_from_archives(:1138 sibling)
          + IncrementalRecovery(per-window+fail-closed); (d) regime guard in WalWriter::open; (e) flip's
          ensure_overlay_wal_regime; (f) prune Owned-pin; (g) §7.1 guards
          [Overlay forward-only PER CHAR FILE; migrate sole bridge; VERSION NOT bumped ⇒ base/vocab/un-flipped-char
           readable by ANY binary forever]
DG-MIGRATE migrate (Owned→Overlay) + CLI                                                        [forward]
DG-TX     tx-gating in reconcile (R6)                                                           [forward]
DG-FORMAL TLA + all controls                                                                    [HARD GATE]
DG-SOAK   ≥50× real-disk + new scenarios incl. idempotent-no-op-vs-concurrent-remove (§A)       [reversible until flip flag]
```
Reversibility: DG0-DG2 code-reversible (fields→0=Owned; VERSION never moves). DG-RECON irreversible ONLY per
char-Overlay file. **Unlike D2.6, no global VERSION bump ⇒ base/vocab + un-flipped char readable by any binary
forever**; the one-way boundary is per-file (Overlay), not global.

### Critical Files
- `wal/header.rs` (+rank_regime byte 28; VERSION stays 2 — the F1 fix); `wal/writer.rs` (regime guard `open:116`;
  rotate/truncate carry regime+floor; prune Owned-pin); `core/recovery.rs` (reconcile_lww regime-keyed filter;
  IncrementalRecovery per-window; rebuild_from_wal_segments `_reconciled` sibling; redo_phase base UNCHANGED);
  `char/lockfree_cas.rs` (4 real producers claim @ loop-top; idempotent arms NO-RANK §A; try_increment_cas_inner;
  merge_lockfree_* reject); `char/recovery.rs` (rebuild_from_wal:503 + replay_wal_after_checkpoint:464 gated).
- Cross-codebase proof sites: `persistent_artrie/mmap_ctor.rs:378`, `persistent_vocab_artrie/mmap_ctor.rs:238` +
  `lockfree_cas.rs:44,98,322` (no-Overlay), `wal/async_writer.rs` + `wal_managed.rs:490` (regime threading).

---

## §A. ORCHESTRATOR CORRECTION — idempotent arms must NOT rank (supersedes §4)

**The bug in §4.** §4 proposed ranking the idempotent arms with a loop-top-claimed `commit_seq`. Code-verified
resurrection: term `t` present; idempotent `Insert(t)` (I) ‖ real `Remove(t)` (R). (1) R claims `cR`, reads root0,
starts building. (2) I claims `cI > cR`, observes `t` final in root0 → `AlreadyExists` → **NO root CAS**
(`build_final_path_recursive` returns `Err(AlreadyExists)` before constructing a root; confirmed "no spine
published" at `lockfree_cas.rs:105`). (3) R's CAS root0→root1 wins (root unchanged by I), removing `t`, with
`cR < cI`. Real-time: `t` absent (R last). Replay sorts `(cseq,lsn)`: `Remove(cR)` then `Insert(cI)` ⇒ `t` PRESENT
= **resurrection; acked remove lost.** The loop-top claim does NOT prevent it because the no-op leaves the root
unchanged, so a lower-`commit_seq` remove can still win its CAS AFTER the no-op. (Under the OLD `root.version()` the
idempotent generation was read AFTER observing present, causally tracking the observed root, so a later remove's
bumped version correctly won — `:381-384` "harmless" was valid there; the GLOBAL loop-top `commit_seq` breaks that
causality.)

**The fix.** The idempotent `AlreadyExists`/`AlreadyAbsent` arms do **NOT** append a `CommitRank` and do NOT claim
a `commit_seq`. Their pre-appended data record (`Insert`@`:329` / `Remove`@`:520`) is then UNRANKED ⇒ in an Overlay
file it is DROPPED by §1.5 ⇒ zero replay effect ⇒ matches the real-time no-op. Preferred implementation:
**read-before-append** — check membership BEFORE the WAL append (a lock-free `find_leaf` read); if the op is a no-op
(present-for-insert / absent-for-remove), return `Ok(false)` WITHOUT appending any record (no spurious record, no
watermark gap). If read-before-append is not adopted, the fallback is **mark-but-no-rank**: keep the pre-appended
record, `mark_committed(lsn)` for watermark liveness (so the contiguous prefix doesn't stall), but DO NOT
`append_commit_rank` ⇒ the record drops at replay. (D2.6 §3.4's "no rank, no mark_committed" was slightly wrong —
no-mark stalls the watermark; the correct combo is mark-yes, rank-no, OR read-before-append.)

**Why this is correct + closes F4.** A no-op asserts STATE, not a MUTATION; it must not durably record a mutation.
With no rank, the no-op has no `commit_seq`, so no inversion is possible (F4's A.5 class is structurally
impossible — there is no idempotent commit_seq to mis-order). The real producers (4 arms that win a CAS) keep the
loop-top-claim+rank (correct — their CAS is the single LP, `CommitSeqMonotone` holds). `IdempotentNoInversion`
(§8) becomes trivially true. Negative control `_UnsafeIdempotentRanked` (rank the idempotent arm) MUST fire
`NoResurrectionOnReplay` via the trace above. New soak scenario: idempotent-no-op-of-`t` ‖ concurrent-real-remove-
of-`t`, crash, reopen ⇒ `t` absent (not resurrected).

**Cross-check vs the current code (`:375-391`, `:556-580`):** today both arms rank with `root.version()` + mark.
DG1 changes them to NO-RANK (read-before-append or mark-but-no-rank). This is a behavior change gated by DG1
(reversible) and must regress-test the existing RB3 `Contains` proptest (`:446`) that asserts the no-op semantics.
