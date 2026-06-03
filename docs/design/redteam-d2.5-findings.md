# Red-team of D2.5 — findings (2 focused adversarial Plan agents, 2026-06-02)

Attacking `docs/design/durable-global-commit-sequence-redesign-d2.5.md`. D2.5's (1a) write-path spine remains
PROVEN (carried from D2, untouched). This round targets the NEW machinery D2.5 introduced (R1c regime, R1d
archive, R2 reclaimed-set floor, R1a per-op merge). **Headline: D2.5's R1c regime mechanism (regime INFERRED
per-checkpoint-window) has multiple CRITICAL holes — regime must be DURABLE + INTRINSIC, not inferred.** The
fix direction (D2.6) is clear and bounded; it does NOT touch the (1a) spine.

---

## RT-D2.5-A — regime machinery (R1c) + archive (R1d) + cross-codebase [agent ab1a186f — LANDED]

**ROOT CAUSE (unifying A's criticals): D2.5 makes `regime` a per-WINDOW property INFERRED from context
(presence of a CommitRank above `checkpoint_lsn`, or a per-Checkpoint stamp). Inference fails at exactly the
boundaries: the first/never-checkpointed window, the multi-regime archive, and cross-restart (regime not
persisted).** Regime must be a DURABLE, per-record (or per-segment-header) INTRINSIC property.

**A#1 [CRITICAL] — first-window regime UNDEFINED.** A v3 overlay trie that wrote ranked records but NEVER
checkpointed has no bounding Checkpoint ⇒ the per-checkpoint stamp (S#1) has nothing to read. The `else`
inference (`any CommitRank DATA_LSN > checkpoint_lsn ⇒ Overlay`) FAILS for a window with ZERO ranks (e.g.
increment-only — which emits no rank today, A#8; or all-writes-mid-two-append-at-crash) ⇒ inference yields
`Owned` ⇒ KEEPS overlay orphans (silent resurrection of never-acked first-window writes). S#1 relocates the
gap to "no checkpoint exists yet," does not close it. Cite `mmap_ctor.rs:301,312,320`, `d2.5 §1.3:155-157`.

**A#2 [CRITICAL] — FALSE PREMISE: the base `redo_phase`/`RecoveryManager` NEVER calls `reconcile_lww`.**
`rg reconcile_lww src/persistent_artrie/` is EMPTY; `mod.rs:262` re-exports the MODULE, not a live call.
`redo_phase` (`recovery.rs:756-833`) runs its OWN raw in-order loop. So D2.5 §0's "shared by BOTH codebases"
is factually wrong, and TODAY a v3 WAL recovered via the base `RecoveryManager::recover` applies overlay
orphans RAW (orphans WIN). **Resolution direction:** the base codebase recovers OWNED-regime tries only
(in-order, already correct); the header-regime + R5 version guard must PREVENT a base ctor from opening an
overlay-regime file (and vice-versa) — they recover different trie types, so unify WITHIN char, not across.

**A#3 [CRITICAL, highest-value] — the multi-regime archive is NOT handled; union-reconcile applies ONE regime
to ALL segments.** Across an Owned→Overlay flip: segment 1 = Owned (unranked, must KEEP), segment 5 = Overlay
(orphans must DROP). With `checkpoint_lsn=0` the union's Overlay ranks ⇒ inference returns Overlay ⇒ the
v3-drop applies to ALL segments ⇒ **every unranked Owned record in segment 1 DROPPED (total loss of pre-flip
owned history)**; inverting ⇒ resurrects segment-5 orphans. The per-checkpoint stamp doesn't save it (union
collapses windows into one regime decision). Cite `d2.5 §1.4:201-202`, `§1.2:106`.

**A#4 [CRITICAL] — base owned checkpoint writes NO regime stamp.** `WalRecord::Checkpoint` (`codec.rs:120,280,449`)
has only `{checkpoint_lsn, timestamp}`. S#1's `regime: u8` is unimplemented AND wired only to the char publisher
(`persist.rs`); the base owned checkpoint (`persist.rs:152`, different codebase) is not a stamp site.

**A#5 [HIGH] — REGIME-CHECKPOINT assert uses the WRONG domain.** The proposed `next_lsn-1 == checkpoint_lsn`
assert in `set_overlay_write_mode` assumes the OWNED domain, but the overlay checkpoint's `checkpoint_lsn` is a
WATERMARK ≤ `next_lsn-1` (the overlay RETAINS un-reclaimed tail > watermark, `persist.rs:579`). So the assert
FIRES on a legitimate Overlay→Owned flip (or, relaxed, lets a gap through). §1.3 only reasoned Owned→Overlay.
Cite `persist.rs:153,568,579`.

**A#6 [HIGH] — Overlay→Owned flip + crash reopens in Owned mode reading the overlay-ranked tail as Owned (KEEPS
orphans); regime is NOT persisted across reopen.** `mmap_ctor.rs:349` resets `overlay_write_mode` to default
(Owned) on EVERY open; regime is operator-set each restart, never read from disk. §8 S#1 defends only the
Owned→Overlay hot-toggle direction. **This is the core argument for durable-header regime.**

**A#7 [HIGH] — base `BatchInsert` (one LSN, N terms) amplifies a regime mis-call: one mis-classification drops a
whole tx/document atomically** (`recovery.rs:343-346` expands to N same-LSN ops). Compounds A#1/A#3.

**A#8 [HIGH, R3 live gap] — `try_increment_cas_durable` emits NO CommitRank today** (`lockfree_cas.rs` 6 rank
sites are insert/remove/insert-value/upsert; increment absent). So if the v3-drop ships ahead of the increment
rank (DG ordering), every acked increment is unranked ⇒ DROPPED. D2.5 R3/DG1 adds it — sequencing must hold.

**A#9–A#11 [MED] —** S#3 contiguity check undefined for an empty/zero-record segment (`writer.rs:415`); the
archive inference is degenerate at `checkpoint_lsn=0` (`DATA_LSN > 0` always true ⇒ always Overlay — the
mechanism behind A#3); the clean forward Owned→Overlay crash case IS sound (documents the boundary).

**Confirmation vs §8 hardenings: S#1 BROKEN (no checkpoint in first window; only char-wired; collapsed by archive
union), S#2 no-op for the archive (cp=0), S#3 incomplete (empty-segment).**

### → D2.6 fix direction for the regime cluster (clear + bounded; does NOT touch the (1a) spine)
**Make regime DURABLE + INTRINSIC, not inferred:**
1. **Regime is a WAL HEADER field** (per file/segment), set at file creation, immutable for the file's life;
   each archive segment's header carries its own regime. Closes A#1 (first window has a header), A#6 (durable
   across reopen), A#4 (header not a separate Checkpoint stamp).
2. **A mode flip FORCES a new WAL file** (checkpoint+rotate the old, create new with the new regime in its
   header). Replaces the wrong-domain REGIME-CHECKPOINT assert (A#5) with "flip ⇒ new file" — cleaner.
3. **Recovery applies the drop rule PER-RECORD using that record's SEGMENT-header regime, with a GLOBAL rank map
   (built across all segments) for cross-segment rank visibility.** Closes A#3 (multi-regime archive: each
   segment keeps its own regime), A#10 (no cp=0 inference).
4. **The base (owned) codebase recovers owned-regime files in-order (unchanged); the header regime + R5 version
   guard prevent cross-codebase mis-opening.** Closes A#2 (don't force base through reconcile; guard instead).
5. Increment-rank (A#8) lands in DG1 before the drop in DG-RECON (sequencing already planned).

---

## RT-D2.5-B — per-op merge (R1a) + floor (R2) + ack (R3) + §8 residuals + DG seq [agent ac0c7cee — LANDED]

**B#1c [resolved by ORCHESTRATOR code-read — C#3b was a MISREAD; reconcile already applies-all-in-order].**
`reconcile_lww` (`recovery.rs:253-298`) builds the rank map (Pass 1), then expands EVERY in-scope record,
stamps each with `generation_of(lsn)=rank.unwrap_or(lsn)`, pushes ALL to `stamped`, STABLE-sorts by
`(generation,lsn)`, and returns ALL (`:287-297`). **NO per-term collapse.** Increments are all emitted and the
applier accumulates them; LWW for membership/value is EMERGENT from apply-all-in-order. ⇒ **R1a (increments-sum)
is ALREADY satisfied; D2.5 §1.1's "reset-point applier" is a fiction — the correct statement is simply
"apply all KEPT records in (commit_seq,lsn) order"; the drop rule only FILTERS which are kept.** This collapses
R1a to a description fix + the filter.

**B#1c-residual [CRITICAL, PRE-EXISTING] — `result==0` sentinel collision.** `recovered_operations_from_record`
(`:347-354`) emits `BatchIncrement → Increment{result:0}` (delta/accumulate); a non-batch `Increment` carries
`result:new_value` (absolute). A signed delta landing a counter at 0 emits `Increment{delta,result:0}` ⇒ applier
(`mutation_core.rs:326`/`persistent_artrie/mutation_core.rs:405`) hits the `result==0` ACCUMULATE arm ⇒ an
absolute-0 is misclassified as a delta ⇒ counter divergence. Pre-existing; D2.6 must fix the encoding (explicit
is_absolute flag / `Option<i64>` result, not the 0-sentinel).

**B#5 [CRITICAL — confirms C#5a] DG1 crash-window REAL.** DG1 ships single-LP insert + claim-commit_seq +
increment-rank at VERSION=2 with the OLD ungated reconcile; an unranked Insert-orphan (data@`:329` before CAS@`:367`,
crash between) sorts at `generation_of=lsn` (LARGE) ⇒ WINS ⇒ resurrects a removed term. DG1's "still-v2 reader
works" holds only for RANKED records. **Fix: the producer change + the gated reader + header bump must be ONE
ATOMIC GATE** (a v3 record can't exist before its gating reader). B#3a reinforces: the CommitRank.generation
carries `root.version()` today (`:363` etc.); repurposing to a global commit_seq is an 8-site producer rewrite,
incoherent to split across DG1/DG-RECON.

**B#3a [HIGH, mechanical] `try_increment_cas_durable` can't stamp a generation as written** — it reuses
`try_increment_cas` verbatim (`:1473`) which captures the root generation INTERNALLY and returns only `new_val`;
the §3 `append_commit_rank(lsn,key,generation)` won't compile (`generation` out of scope). Must thread it out
(or inline, touching the formally-checked no-lost-update reuse). Value-path IS single-LP (`:1441`) so
`CommitSeqMonotone` holds once generation is sourced.

**B#2a [HIGH] floor-carry is MANDATORY, not optional.** The OWNED checkpoint TRUNCATES/rotates the WAL
(`persist.rs:170`→`writer.rs:458` fresh `WalHeader::new()` drops the floor), so a "recompute the map from the WAL
scan" fallback (S#4) UNDER-computes (the reclaimed ranks are now in an archive, gone from the active WAL). ⇒ the
header-floor-carry across rotate/truncate (`writer.rs:458/:353`) is the ONLY thing preventing floor regression —
it is load-bearing, not a nicety. **B#2b [MED-HIGH]** Overlay→checkpoint→reopen-Owned: the overlay RETAINS its
WAL (`persist.rs:599`), so overlay ranks are still active at the next owned checkpoint ⇒ `range(..=next_lsn)`
yields a non-zero floor from overlay seqs the owned image LACKS ⇒ the B#4c domain-mismatch RESURFACES at the
switch-back seam. **B#2c [MED]** the `commit_seq_by_data_lsn` map grows unbounded between rare/absent checkpoints
(overlay retains WAL; never-checkpoint soak) — memory unbounded.

**B#4-residuals [MED] —** `migrate_v2_to_v3` is VAPORWARE (cited as the one-way bridge / C#6 answer; `rg` finds 0
occurrences) ⇒ the rollback story rests on a non-existent tool. `IncrementalRecovery` (`recovery.rs:932-969`) is
fully ungated (no rank, no sort, no drop) and §8 never re-attacks it — the most-exposed never-checkpoint-v3 path.

**B verified SOUND:** S#7 (group-commit rank-sync — Err-not-ack), S#5 (increment+remove narrowly), S#4 (overlay-
retain only), #1d (V=() degenerates — increment producers are u64-monomorph-only `:1787`).

---

## SYNTHESIS → D2.6 (materially SIMPLER; the regime cluster collapses to "regime ≡ version")

The (1a) write-path spine is PROVEN (untouched). The reconcile is ALREADY apply-all-in-order (R1a ≈ no-op). The
two cycle-3 mechanisms that broke (inferred regime + split DG) both simplify:

**D1 — regime ≡ WAL VERSION (collapses A's entire cluster).** A v3 WAL is PURE overlay (all confirmed ops ranked);
v1/v2 is owned/legacy (unranked-norm). **No mixed v3 WAL** — the flip creates a FRESH v3 WAL, the owned path never
writes v3 (enforced by the R5 `WalWriter::open` version guard), so v3 ⟺ overlay-regime. The drop is purely
version-gated: v3-unranked ⇒ DROP (orphan), v1/v2-unranked ⇒ KEEP@lsn (legacy). NO regime field, NO per-window
inference (closes A#1 first-window, A#6 cross-restart). **Archive: drop PER-SEGMENT by that segment's header
VERSION** (v2 segments keep, v3 segments drop), one global rank map for cross-segment visibility (closes A#3
multi-regime archive). Base codebase recovers owned (v2) files in-order, unchanged; the version guard prevents
cross-opening (closes A#2 — don't force base through reconcile). Floor lives only in v3 files ⇒ one domain ⇒
B#2b switch-back domain-mismatch VANISHES (owned=v2 file, overlay=v3 file, separate).

**D2 — reconcile: keep apply-all-in-(commit_seq,lsn)-order; ADD the version-gated drop FILTER** (R1b). No
reset-point coder (B#1c). Fix the `result==0` sentinel (B#1c-residual). Gate `IncrementalRecovery` + the archive
through the same filtered reconcile (B#4).

**D3 — ONE ATOMIC GATE** for {header 2→3, single-LP insert, commit_seq stamp (8 sites), increment-rank+generation
threading, gated reconcile reader, version guard} — a v3 record never exists before its reader (B#5/B#3a).

**D4 — floor: header-carry MANDATORY across rotate/truncate** (B#2a, the long-standing ProcessRule); per-v3-file
domain; bound the `commit_seq_by_data_lsn` map (B#2c — e.g. cap + fall back to scan, or prune by a low-watermark).

**D5 — IMPLEMENT `migrate_v2_to_v3`** (B#4) — recover-under-v2 → checkpoint to fresh v3; the sole one-way bridge.

**D6 — fix the increment `result==0` encoding** (B#1c-residual) + thread the increment generation (B#3a).

This is SIMPLER than D2.5 (no regime field/inference, no per-op-merge coder, reconcile mostly as-is). The (1a)
spine + R3 ack + R5 chokepoint + R6 tx + R7 errata carry from D2/D2.5. Next: a Plan agent produces D2.6 with these
6 decisions pre-made (filling in mechanics, not re-deriving), then ONE final narrow red-team (regime≡version +
atomic-gate + result-sentinel), then foreground implementation.
