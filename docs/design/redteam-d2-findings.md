# Red-team of D2 — findings (3 adversarial Plan agents, 2026-06-02)

Attacking `docs/design/durable-global-commit-sequence-redesign-d2.md`. D2 self-red-teamed (§6) and caught
two sink-the-design seams (§6.2 builder-split, §6.4 reconstructed-watermark); this external round verifies
those fixes AND hunts what §6 missed. **Verdict forming: D2's spine (the (1a) single-LP correction + the
durable floor) SURVIVES; it needs a D2.5 ERRATA — local corrections + one omitted mechanism, NOT a
redesign.** Findings below; corrections synthesized at the end once all three land.

---

## RT-D2-B — Axis 2 (floor) + Axis 3 (watermark drop-rule) [agent a18c4b8a — LANDED]

**B#1 — §3.2 drop-rule pseudocode is INTERNALLY CONTRADICTORY (pseudocode = total loss; prose = sound).
CRITICAL-if-shipped-as-written; trivially fixed.** The §3.2 pseudocode has BOTH `None` arms as `continue`
(drop). Taken literally, a v1/v2/in-order WAL where NO record is ranked ⇒ every record `None` ⇒ every record
dropped ⇒ **total data loss on the legacy/core path.** BUT §3.2-prose + §3.3 ("in-order ⇒ pass `Lsn::MAX`")
+ §5.3 (version-select) specify the correct behavior. **The reconciliation (the fix): the
`None if lsn <= committed_watermark` arm must KEEP with `generation_of = lsn` (legacy LSN-order), NOT
`continue`.** Only `None && lsn > committed_watermark` drops (the v3 two-window orphan). With `Lsn::MAX` for
in-order paths, every unranked record is `≤ MAX` ⇒ kept in LSN order = exactly today's
`recovery.rs:273 .unwrap_or(lsn)` behavior, no loss. **MUST be reconciled before DG3/DG4.** (This was the
orchestrator's pre-flagged candidate — confirmed.) Cite: doc §3.2 L192-196,200 vs L204-205,260-262;
`recovery.rs:273,283-285`.

**B#4c — floor↔flip cross-restart loss via owned-vs-overlay `checkpoint_lsn` DOMAIN MISMATCH. HIGH; D2 §6.3
self-fix is a DEFERRAL, not an elimination (violates D2's own "no deferrals" standard).** The two checkpoint
paths write `checkpoint_lsn` from DIFFERENT LSN domains: owned-tree `publish_durable_and_reclaim` uses
`self.next_lsn` (`persist.rs:153`, the owned-mutation counter the lock-free path NEVER advances —
`:408-411,460-462`); the overlay path uses `watermark` (`:568`). D2 §2.5 sets `floor = max_durable_commit_seq`,
which is bumped by EVERY durable producer INCLUDING overlay writes. If overlay writes bump
`max_durable_commit_seq` but the checkpoint runs the OWNED-tree path (whose image lacks those overlay writes),
the floor claims seqs are subsumed that AREN'T in the image ⇒ after reopen the owned image seeds without the
overlay writes, the floor pushes new seqs above them, and reconcile can sort a replayed-overlay-write below a
stale owned-image value ⇒ **acked overlay write lost.** D2 §6.3's mitigation is PURE SEQUENCING PROSE ("DG2
lands with/after F3"), no code-level guard. **Principled fix (D2 omits): source the floor from the
ACTUALLY-RECLAIMED set bounded by THIS checkpoint's `checkpoint_lsn` — i.e. `floor = max commit_seq among
records with data_lsn ≤ checkpoint_lsn` — NOT the global `max_durable_commit_seq`.** That ties the floor to
the reclaim boundary, domain-correct for both owned and overlay checkpoints; un-reclaimed overlay writes
(higher seq, data_lsn > checkpoint_lsn) keep their WAL records for replay and are not falsely subsumed.
Cite: doc §2.5 L165-169, §6.3 L279-281; `persist.rs:153,315,408-411,460-462,568`.

**B#2 — ranked-above-hole VERIFIED SOUND; unranked-acked-BELOW-watermark CONFIRMED a real gap (MED-HIGH).**
First half SOUND: §3.2's `Some(s)` arm fires on ANY ranked record regardless of watermark ⇒ an acked ranked
write above an unranked hole is NEVER dropped (watermark is a floor for *unranked* drops, not a ceiling for
*ranked* keeps). Converse REAL: §3.2 calls unranked-`≤`-watermark "impossible/anomaly LOSE", but under
group-commit the rank record (a SEPARATE later-LSN append, `wal_helpers.rs:87`) can physically trail its data
record; if the data+mark synced but the rank is in a later un-synced batch, recovery sees unranked data and
the "anomaly LOSE" arm drops it. **Correctness hinges on ACK ORDERING the design never states explicitly:
the caller must be acked ONLY AFTER the rank record is durable** (then acked ⟹ rank durable ⟹ never the
trailing-rank case ⟹ safe). D2 must state "ack-after-rank-sync" as an invariant (and the `≤watermark`-arm-keep
from B#1 makes the unranked-below case keep-in-LSN-order anyway, closing it). Cite §3.2 L194, §6.4 L285,
`wal_helpers.rs:87`, `lockfree_cas.rs:367-372`.

**B#3 — reconstructed-watermark computation UNDER-SPECIFIED (MED).** "largest L s.t. every data-LSN in
(cp,L] is ranked" — but rank records OCCUPY interleaved LSNs in (cp,L] that are neither data nor "ranked-data";
D2 gives no rule to classify/skip marker-LSNs. A naive contiguous-ranked-prefix impl either stalls at the
first rank-LSN (everything above drops, a milder B#1) or needs a two-pass classify D2 never specifies. Also a
dangling rank (CommitRank whose data_lsn was checkpoint-truncated) pollutes the `rank` map (harmless for the
prefix but unspecified). **The watermark reconstruction is NAMED, never ALGORITHMIZED.** Fix direction:
either (a) precisely specify the marker-LSN-skipping two-pass walk, or (b) PERSIST the committed watermark
durably (like the floor, in the header) so recovery reads it instead of reconstructing the fragile
rank-interleaving. Cite §3.2 L200, §6.4 L285, `recovery.rs:259-271`, `committed_watermark.rs`.

**B#5 — DG1→DG3 intermediate-state comparator SKEW (MED; sequencing).** DG1 stamps `commit_seq` into
CommitRank (and ranks increments) but reconcile keeps reading it as root-version until DG3 ⇒ in the
DG1/DG2-only state a v3 WAL's `commit_seq` (small global counter) is mis-read as a root-version/generation and
mis-sorts a ranked increment vs a ranked insert. D2 §8 sequences drop-rule-after-increment-rank but NOT
comparator-WITH-stamp. **Fix: defer the header `2→3` bump (and thus the v3-comparator selection) to land
TOGETHER with the new reconcile reader (DG3), OR move the stamp to DG3 — the stamp's meaning and the reader's
interpretation must flip in the same gate.** Cite §6.5 L287-289, §8 L348,350,356, `lockfree_cas.rs:1536-1553`.

**RT-D2-B verified SOUND:** ranked-dominates-watermark (B#2 first half); floor monotonicity under
multi-checkpoint + rotate-without-checkpoint (B#4a/b — the §2.3 carry correctly closes the fresh-header trap);
commit_seq-gaps-vs-data-LSN-gaps not conflated (B#3a).

---

## RT-D2-A — Axis 1 ((1a) insert + CommitSeqMonotone) [agent a22e5fb3 — LANDED]

**HEADLINE: `CommitSeqMonotone` and the no-prefix-regression proof CANNOT be broken — both HOLD under (1a)
as specified.** The architectural crux survives. D2's §6.2 builder-split + §6.4 reconstructed-watermark
self-fixes are CONFIRMED necessary and load-bearing.

**A — VERIFIED SOUND (could not break):**
- **A1c [the headline]** `as_final().store.clone()` cannot lose a child to concurrent tier-growth — the
  premise is FALSE: there is NO in-place tier growth of a shared node. `store` Inline→Heap promotion happens
  only inside `with_child`, which builds a FRESH node published by root CAS (`node.rs:420-465`); the only
  in-place mutation anywhere is `try_set_final`'s flag `fetch_or` (`:724`), which touches no child. A racer's
  child-add ⇒ different node ⇒ CAS `expected` mismatch (Arc::ptr_eq) ⇒ Conflict ⇒ retry re-clones the grown
  store (§1.3 Case B). No losing interleaving.
- **A1a/1b** deeper-nesting + 4-way same-spine race + None-arm fresh-spine compose without loss (the §1.3
  proof generalizes; root CAS is a total order; None-arm bakes `as_final` ONLY into the leaf, intermediates
  stay non-final).
- **A3** `CommitSeqMonotone` adjacency HOLDS despite the data-append-between — because the claim is
  per-iteration at the loop top; the data append (`:329`) is BEFORE the loop, never between a claim and its
  CAS. No same-term `X≺_CAS Y` with `commit_seq(X)>commit_seq(Y)` is constructible.
- **A5(attack)** non-durable `insert_cas` ‖ durable `insert_cas_durable` on one `V=()` instance do NOT
  corrupt finality (membership is monotone 0→1; a `try_set_final` on a CAS-orphaned node is a benign dead flip).
- §6.2 builder-split CONFIRMED REQUIRED (a shared finalizing builder WOULD reintroduce the Phase-A "d after da"
  bug, `:791-797`).

**A#5 — [CRITICAL if DG4 slips] archive/rebuild path RESURRECTION — the orphan `Insert` is NOT dropped on the
archive path.** §3.4's drop rule lives ONLY in `reconcile_lww`, but `recover_from_archives` (`mmap_ctor.rs:1138`)
→ `rebuild_from_wal_segments` (`recovery.rs:1443`) replays EVERY data record in raw LSN order via
`apply_core_recovered_operation_no_wal` (`mutation_core.rs:310`, unconditional `insert_impl_no_wal`) — NO
reconcile/rank/watermark/tx-gate. Trace: an idempotent-arm orphan `Insert("k")`@lsn11 (unranked, AlreadyExists)
in a retained segment, AFTER `remove("k")`@lsn9, both archived ⇒ raw-LSN replay applies Remove(9) then
Insert(11) ⇒ **"k" RESURRECTED ≠ live-absent.** **D2 §3.3's "archives hold only confirmed records" claim is
FALSE for idempotent-arm orphans** (durable+unranked, CAN be in a retained pre-checkpoint segment). And §3.3's
`Lsn::MAX` watermark for archives makes the drop rule a no-op there. Fix: DG4 must route archives through
reconcile AND **drop unranked records in archives** (version-gated, see synthesis). Cite `recovery.rs:1443-1487`,
`mmap_ctor.rs:1138`, `mutation_core.rs:310`.

**A#6 — [confirms §6.4] reconcile's drop rule is INERT until the reconstructed watermark is implemented; today
NO watermark/version/tx_states param exists** (`replay_records_lww:252`, both ctors `:403`/`:227` pass only
`(ops, loaded, checkpoint_lsn)`). Runtime `CommittedWatermark` reopens seeded to the FULL frontier ⇒ if reconcile
read it, it would gate nothing. §6.4's RECONSTRUCT-in-scan is necessary AND absent (expected at DG3).

**A#7 — [design ambiguity, MED] the data-LSN→commit_seq binding must be stated.** The Insert/Remove DATA record
carries NO commit_seq; commit_seq lives ONLY in the CommitRank, appended once in the CAS-win arm. DG1 must state
this explicitly so an implementer does NOT hoist the claim beside the `:329` append (which would break A3's
adjacency or force per-retry re-append, forbidden by §1.4).

**A#4-residual — [MED] the "non-durable is replay-irrelevant" premise is load-bearing + UNTESTED.** No
invariant/test forbids a future durable+non-durable mix where a non-durable `try_set_final` (in-place 0→1) races
a durable `as_non_final` (copy+CAS 1→0) on a shared node — safe TODAY (remove uses copy+CAS) but the premise
should get a guard/test.

## RT-D2-C — Axis 4/5 + unification + DG sequencing [agent ac95607a — LANDED]

**C — VERIFIED SOUND:** §4.3 torn-interior "no strict-subset" (C#1a — but via per-record CRC + fresh-append
holes-read-as-zeros, NOT the watermark; `group_commit.rs:662` N-appends+1-sync, `reader.rs:51,77`); per-op
contiguous-prefix blocking (C#2, `committed_watermark.rs:65`); aborted-tx burned seqs harmless (C#9).

**C#3b — [HIGH] routing `redo_phase` through `reconcile_lww` BREAKS existing in-order recovery — increments are
NOT summed.** `redo_phase` (`recovery.rs:756`, ignores `_checkpoint_lsn`/`_transactions`) today emits ops in
strict LSN order and applies every record (increments ACCUMULATE). `reconcile_lww` LWW-collapses N same-term
records to the single `(commit_seq,lsn)` winner and **DROPS intermediates** (`mutation_core.rs:285`). For
increments this is WRONG: deltas must SUM, not LWW-pick — reconcile keeps only one, silently dropping the rest.
**Root insight: LWW is correct for membership/value, WRONG for commutative increments.** Cite `recovery.rs:756-834`
vs `:288-298`.

**C#4 — [HIGH] archive unranked-orphan + `Lsn::MAX` (deeper A#5).** `rotate_to_archive` renames the ENTIRE active
WAL (`writer.rs:424`) incl. two-append-window orphans (data durable `:329`, rank not yet appended). The checkpoint
assert (`persist.rs:140-146`) only checks `next_lsn` didn't move — NOT that every record ≤ cp is ranked. §3.3's
`Lsn::MAX`-for-archives conflates "subsumed by a checkpoint that wrote a BASE image" with "present in these
segments"; `recover_from_archives` rebuilds from segments ALONE with no base ⇒ archived-but-unranked orphans are
reachable and (with `generation_of=lsn`) WIN. Cite `writer.rs:424-470`, `persist.rs:140-146`, `recovery.rs:1443`.

**C#5a — [HIGH] DG1→DG3 crash-window is NOT data-safe (sharpens B#5).** DG1 bumps header 2→3 + writes per-op
`commit_seq` ranks, but DG3 lands the gated reconcile. Between DG1- and DG3-deploy, a v3 WAL is recovered by the
CURRENT ungated `reconcile_lww` (`recovery.rs:253`, NO watermark/version/tx params today) → unranked two-window
records get `generation_of=lsn` and WIN = the exact loss class DG3 exists to kill. **DG1 is not a green/safe
intermediate.** Cite `recovery.rs:253-256`, §8.

**C#6 — [MED-HIGH] header 2→3 is NOT "reversible by code".** `from_bytes` refuses `version > VERSION`
(`header.rs:82`); a v2 binary (reverted DG1) refuses a v3 file ⇒ user STRANDED. §8 mis-files the one-way bump
inside the reversible DG0–DG5 band.

**C#7 — [HIGH] the version-refuse guard chokepoint is WRONG — base ctors bypass it.** D2 §5.2/§6.7 place the guard
at `wal_managed::open_or_create_async_wal` (char/vocab only). The BASE `PersistentARTrie<V>` ctors call
`AsyncWalWriter::open_or_create`/`create` DIRECTLY (`persistent_artrie/mmap_ctor.rs:378`, `io_uring_ctor.rs:66`),
bypassing it ⇒ base-dict write path appends v3 to a v2 file. **Guard must live in `AsyncWalWriter::open_or_create`
itself.** Cite those two ctors.

**C#8 — [HIGH] per-op tx commit_seq premise is FALSE.** `commit_document` (`document_tx.rs:215`) logs the ENTIRE tx
as ONE `BatchInsert` with ONE LSN for ALL N terms + one `CommitTx`, with NO CommitRank. So "per-op commit_seq in a
tx" has no per-op record to carry it without splitting `BatchInsert` (a WAL-format change D2 never specifies).
§6.6's defense is moot. **Fix: ONE commit_seq per BatchInsert/tx (atomic unit), increments summed — simpler AND
correct.** Cite `document_tx.rs:215-244`, `recovery.rs:343-348`.

**C#10 — [MED] migration v2-read under the unified drop-rule under-specified** (v2 CommitRank carries root-version,
a different domain from the watermark reconstruction; §5.3 doesn't reconcile them). **C#11 — [LOW] v1 fallback
sound-if-implemented** (v1 must wire to `unwrap_or(lsn)`, NOT the drop-rule). **C#12 — [MED] DG2 floor↔flip: safety
actually preserved by "floor-too-high-is-harmless" (monotone), but D2's stated justification "floor==scan pre-flip"
is FACTUALLY WRONG once DG1 overlay ranks exist** — the real reason it's safe must be argued (and B#4c's
floor-from-reclaimed-set makes it robust).

---

## SYNTHESIS → D2.5 (recovery/reconcile/sequencing corrections; the (1a) write-path spine is PROVEN, untouched)

**The headline: RT-D2-A could NOT break the (1a) crux or `CommitSeqMonotone` — the ARCHITECTURE IS SOUND. Every
hole is in the SUPPORTING MACHINERY (reconcile semantics, archive path, floor source, DG sequencing, enforcement
chokepoint, tx encoding).** The loop has converged: D1 broke at the architecture; D2 fixed it; D2's residue is all
tractable machinery. D2.5 corrects the machinery on D2's proven spine. The corrections, grouped by root cause:

**R1 — reconcile semantics (C#3b, A#5/C#4, C#8): the deepest cluster.**
- **Increments SUM, membership/value LWW.** reconcile must merge per-op-type: commutative-delta SUM for
  increments (apply ALL confirmed deltas), LWW-by-`(commit_seq,lsn)` for membership/value. (D2 LWW-collapsed
  everything — wrong for counters.)
- **Version-gate the drop rule.** v3: unranked ⇒ DROP (every v3 confirmed op is ranked, so unranked = orphan);
  v1/v2: unranked ⇒ KEEP in legacy LSN/root-version order (no drop). This subsumes B#1 (the ≤watermark-keep is the
  v1/v2 branch) AND closes A#5/C#4 (v3 archive orphans dropped). The watermark may then be UNNEEDED for the v3 drop
  decision (unranked=drop is version-gated, not watermark-gated) — resolving B#3's under-specification. CAVEAT
  (the transition subtlety): pre-flip the WAL is MIXED (owned-tree in-order unranked + overlay ranked); a v3 file
  with owned-tree records needs those KEPT. So the drop must distinguish owned-in-order-unranked (keep) from
  overlay-orphan-unranked (drop) — candidate: a per-record "expects-rank" bit, OR the contiguous-ranked-prefix
  watermark after all (B#3), OR don't share a WAL across owned+overlay. **This is the #1 design question for D2.5.**
- **Archive path:** route `rebuild_from_wal_segments` through the same reconcile (collect all segments into one
  Vec for cross-segment rank visibility) with the v3 drop applied; recover_from_archives has no base, so the
  segment set must be self-complete.

**R2 — floor source (B#4c/C#12): `floor = max commit_seq among records with data_lsn ≤ checkpoint_lsn`** (the
actually-reclaimed set), NOT the global `max_durable_commit_seq`. Domain-correct for owned AND overlay checkpoints;
eliminates the §6.3 deferral.

**R3 — durability/ack invariant (B#2): ack the caller ONLY AFTER the CommitRank is durable (synced).** Then
acked ⟹ ranked ⟹ never dropped. State explicitly.

**R4 — DG resequencing (B#5/C#5a/C#6): the header `2→3` bump + the new gated reconcile reader MUST land in ONE
atomic gate** (never write a v3 record a deployed reader can't gate). The header bump is **one-way** — stop
claiming it reversible; provide the `migrate_v2_to_v3` tool as the only cross-version path.

**R5 — enforcement chokepoint (C#7): move the version-refuse guard INTO `AsyncWalWriter::open_or_create`/`open`**
(not the char/vocab wrapper), so the base `PersistentARTrie<V>` ctors are covered.

**R6 — tx (C#8): ONE commit_seq per `BatchInsert`/tx (atomic unit)**, increments summed (R1). Drop the
unimplementable per-op-in-tx scheme.

**R7 — local errata (A#7, C#10, C#11, A#4-residual): state commit_seq-lives-only-in-CommitRank; specify the v2
root-version reconcile + v1 `unwrap_or(lsn)` branches; add a guard/test for the "non-durable is replay-irrelevant"
premise.**

**None touch the (1a) write path.** D2.5 = a Plan-agent redesign of the recovery/reconcile/sequencing layer
(R1–R7), then RED-TEAMED, then foreground DG-implementation. The R1 mixed-WAL drop-distinction is the load-bearing
open question to nail first.
