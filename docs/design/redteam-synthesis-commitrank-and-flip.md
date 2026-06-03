# Red-team synthesis: CommitRank fix + value-CAS + flip-F0 (3 adversarial Plan agents)

**2026-06-02.** Three independent read-only red-team Plan agents attacked the design. **Headline: the SHIPPED,
COMMITTED CommitRank data-loss fix (`cf1f80c`) is INCOMPLETE — it has multiple residual data-loss holes.** The
50/50 soak + TLA that "verified" it passed only because they didn't exercise the archive-rebuild path, cross-
restart, increment-mixing, or the idempotent-arm race. This vindicates the multi-agent red-team.

## A. CommitRank fix (cf1f80c) — RESIDUAL DATA-LOSS HOLES (red-team #2, code-cited)
1. **[CRITICAL] Only 1 of 4 recovery paths honors CommitRank.** `reconcile_lww` (`recovery.rs:253`) is the ONLY
   generation-ordered consumer. `redo_phase` (`:756`), `IncrementalRecovery` (`:932`), `rebuild_from_wal_segments`
   (`:1443`, used by the **char `recover_from_archives`** `mmap_ctor.rs:1138`) replay in raw LSN/physical order,
   treating CommitRank as a no-op. ⇒ the s019 loss recurs on the archive-rebuild + incremental + recovery-manager
   paths. (Design §3.8 said "REC-B must reuse the reconcile"; never wired.)
2. **[CRITICAL] The generation (root `version`) is NON-DURABLE and resets to 0 every `enable_lockfree`
   (`lockfree_cas.rs:149`), while LSNs are globally durable-monotone (`writer.rs:135`).** ⇒ cross-restart
   generation collision: S1 insert+remove (no checkpoint) → reopen (root v0) → S2 insert@gen1 → crash → reconcile
   sorts the S1 remove@gen2 AFTER the S2 insert@gen1 → **acked S2 insert lost.** Defeats EVEN the fixed
   `reconcile_lww` across a restart. The §3.6 monotonicity holds only within ONE root lifetime.
3. **[CRITICAL] `try_increment_cas_durable` is rank-FREE** (`:1492`, by design F-3) → `generation_of=lsn` (large)
   ⇒ a ranked insert/upsert of the same key (gen=root-version, small) sorts BEFORE a later increment regardless
   of real commit order ⇒ value corruption (upsert(100) then increment(+1) replays as 101 ≠ visible 100). The
   commutative-sum argument holds ONLY among increments; ANY ranked op of the same key inverts. (= red-team #3's F3.)
4. **[HIGH] `reconcile_lww` ignores the tx state machine.** It expands `BatchInsert`/`BatchIncrement`
   unconditionally (`:343-355`), ignoring BeginTx/CommitTx/AbortTx (`:356-370`), so an aborted/crash-incomplete
   document-tx's ops are REPLAYED by the ctor path but DISCARDED by `redo_phase` (gates on CommitTx) ⇒ two paths,
   two recovered states; resurrection of uncommitted data on the ctor path. (Masked today only because overlay mode
   rejects commit_document; OwnedTree mode uses it + opens via the same ctor → reconcile_lww.)
5. **[HIGH] Idempotent `AlreadyExists`/`AlreadyAbsent` arms read generation from a LIVE root re-walk**
   (`lockfree_cas.rs:383,567`: `lockfree_root.load().version()`), NOT the published leaf — the §3.6 stale-read
   hazard the design warned against. A no-op can be stamped with a generation HIGHER than a genuine earlier remove
   → resurrection (narrow race).
6. **[MEDIUM] crash-between-data-append-and-rank-append / `append_commit_rank` failure** → unranked durable record
   (`generation_of=lsn`); benign in isolation, loses in conjunction with 2/3.

**SOUND (verified):** the winning arms of insert/remove/insert_with_value/upsert rank with the ROOT generation from
the exact published root (`:346,532,1660,1758`) — single-root-lifetime correct; the §3.6 leaf-vs-root tie is
correctly avoided in the winning arms.

## B. value-CAS (red-team #1) — C1′ design ready
`compare_and_swap`/`get_or_insert` phantom = the same class. **Ship C1′:** R-1 (read-before-append: decide
resident/match BEFORE appending; the `cas(Some,absent)` branch never appends) + R-2 (rank a bailed orphan with the
**read-snapshot** root generation `g_read`, strictly < any superseder's generation). Lemma proven strict+total
with R-1. Self-red-teamed 6 ways (two-bailers race, remove-superseder, IoError) — handled. Rejected C2 (Order-A
violation), C3 (redundant). Re-proof: `NoPhantomCasWrite` TLA + negative control + loom + proptest. **BUT C1′'s
R-2 generation is also a root-version → it inherits hole A.2 (non-durable, cross-restart).**

## C. flip-F0 (red-team #3)
- **[HIGH] F1: production `checkpoint()` is NOT flipped** (`mod.rs:1283` captures the OWNED tree + `next_lsn` reclaim).
  ⇒ enabling overlay-write-mode (the kill-switch, or F5) WITHOUT flipping the checkpoint (F3) = #41 loss. **F3 is a
  hard prerequisite for F5; the `OverlayWriteMode` kill-switch is currently UNSAFE until F3.**
- **[LOW] F4:** watermark permanently stalls on an Order-A fault-in `IoError` (liveness, no loss).
- **SOUND:** root-CAS linearizability/loser-safety/no-ABA, Order-A ack, #41 capture-ordering (for
  capture_snapshot_immutable), positive-cache invalidation, no F0 deadlock.

## D. THE DEEPEST INSIGHT (the synthesis)
**"generation = per-lifetime root version" is fundamentally wrong as a durable global replay-ordering key.** It is
(a) non-durable + resets to 0 per `enable_lockfree` (A.2), (b) a different number-domain from the LSN fallback used
for unranked records (A.3, A.6), (c) honored by only 1 of 4 recovery paths (A.1). The CommitRank mechanism needs a
PRINCIPLED REDESIGN, not a patch:
- The ordering key must be **durable + globally monotone across restarts** (candidate: a persisted commit-sequence,
  or derive the rank generation FROM the LSN domain so ranked + unranked records share ONE monotone domain — e.g.
  rank carries the *visibility* order as an LSN-comparable value).
- **EVERY state-changing durable record must be ranked** (including increments — A.3) **with that durable key**, OR
  the ordering must be reconstructable uniformly.
- **ALL FOUR recovery paths** must consume the ordering (unify on `reconcile_lww`, or wire the generation into
  redo_phase/IncrementalRecovery/rebuild_from_wal_segments) — A.1.
- The idempotent arms must rank from the published leaf/root they observed, not a live re-walk (A.5).
- `reconcile_lww` must honor the tx state machine (A.4) (= the tx-i follow-on, now mandatory not optional).

## E. RECOMMENDED PATH (rigor-preserving)
1. **Principled redesign of the durable-ordering mechanism** (the A-class holes + B's cross-restart dependency) —
   a Plan agent, then RED-TEAMED again (the user's loop-until-dry rigor). This supersedes the order-a-replay-order
   fix's "generation = root version" with a durable global key + all-paths consumption.
2. Fold in value-CAS C1′ (B) on top of the corrected durable key, F3 checkpoint-flip + F1 gate (C), F4, and the F0
   batch/doc-tx fixes (Q1-A / tx-ii from `f0-hack-fixes.md`).
3. Implement in the FOREGROUND, monitored, green-gated, with the soak EXTENDED to exercise the holes the original
   missed (archive-rebuild, cross-restart, increment-mix, aborted-tx, idempotent-arm race) — the soak's blind spots
   are why these survived.
**Until the redesign lands, `cf1f80c`'s CommitRank fix is a PARTIAL fix (better than nothing — closes the single-
session reconcile_lww path — but with the residual holes above); the flip must NOT proceed.** `db7cb2d`+`cf1f80c`
remain committed; the F0 hacks remain uncommitted/untouched.
