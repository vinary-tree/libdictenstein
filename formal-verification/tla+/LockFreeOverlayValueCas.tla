----------------------- MODULE LockFreeOverlayValueCas -----------------------
(***************************************************************************)
(* PHANTOM-SAFETY of a CONDITIONAL / RECOMPUTED value write on the           *)
(* lock-free overlay (design "C2" — the shared mechanism behind                *)
(* `compare_and_swap` and the new `merge_*` per-key CAS-retry loop).         *)
(*                                                                         *)
(* The Rust component modelled is `compare_and_swap_cas_durable_default`      *)
(* (`overlay/durable_write.rs`) and the C2 merge funnel that reuses it. A     *)
(* conditional value write is Order-A:                                        *)
(*   step 1: read the current value (the CAS `expected`, or the merge basis), *)
(*           then APPEND + sync the `Upsert{new}` record DURABLE *before* the *)
(*           visibility publish (`append_durable_wal`);                       *)
(*   step 2: publish via the root CAS, RE-checking that the current value is  *)
(*           still the basis. If it still matches -> WIN: the record is RANKED *)
(*           (`commit_rank_and_mark`) and becomes visible. If a concurrent    *)
(*           writer changed the value -> the CAS is REFUSED (`NotApplied`):   *)
(*           the caller is told `Ok(false)`, and the already-durable record   *)
(*           is BURNED (`mark_committed_burned` — NEVER ranked).              *)
(*                                                                         *)
(* The hazard this spec defeats is the *append-before-failed-CAS PHANTOM*:    *)
(* a conditional write whose CAS was refused has its `Upsert{new}` record     *)
(* already durable. If that record were RANKED, an Overlay-regime crash       *)
(* recovery would replay it as an overwrite the caller was told FAILED — a    *)
(* phantom / lost-update (an s019-class, value-dependent hazard).             *)
(*                                                                         *)
(* The design closes it: a burned record is left UNRANKED, and an Overlay-    *)
(* regime reopen DROPS unranked records (`recovery.rs:332`                    *)
(* `RankRegime::Overlay => continue`). So a crash-recover reconstructs        *)
(* EXACTLY the live published state — no refused write appears.               *)
(*                                                                         *)
(* `LockFreeOverlayDurableReplay.tla` proves CommitRank replay ORDER for      *)
(* *won* writes, but it never models a durable-but-REFUSED record; this       *)
(* module supplies the missing burn-drop obligation.                         *)
(*                                                                         *)
(* USE_BURN_ON_LOSS = TRUE  -> the design: a refused write's durable record   *)
(*   is UNRANKED, so recovery (`recov`, the last RANKED durable write per     *)
(*   term) is DROPPED -> `recov` stays equal to the live `committed` state.   *)
(*   ALL invariants hold.                                                     *)
(* USE_BURN_ON_LOSS = FALSE -> the `_Unsafe.cfg` NEGATIVE CONTROL: the        *)
(*   "forgot to burn" bug RANKS the refused record. Because that record was   *)
(*   appended AFTER the winner (higher LSN), it becomes the value a reopen    *)
(*   reconstructs -> `recov` diverges from `committed`: recovery resurrects a *)
(*   value the caller was told `Ok(false)`. TLC MUST report a violation of    *)
(*   `NoPhantomConditionalWrite`. If TLC PASSES this configuration, the       *)
(*   negative control is broken -> fail the whole gate.                       *)
(***************************************************************************)

EXTENDS Naturals, FiniteSets, TLC

CONSTANTS
    Terms,            \* the finite set of keys that may be conditionally written
    Vals,             \* the finite set of (non-Absent) values a write may publish
    USE_BURN_ON_LOSS  \* TRUE = design (refused write left UNRANKED, dropped on reopen);
                      \* FALSE = unsafe negative control (refused write RANKED -> phantom)

ASSUME Terms # {}
ASSUME Vals # {}

\* `Absent` (0) is the "no published value" sentinel; it must not collide with a value.
Absent == 0
ASSUME Absent \notin Vals

AbsentOrVal == {Absent} \cup Vals

\* A dummy value to fill the inactive in-flight slot (its fields are ignored when
\* `active = FALSE`); keeps the `pend` record type uniform for TLC.
DummyVal == CHOOSE v \in Vals : TRUE
NonePend == [active |-> FALSE, val |-> DummyVal, basis |-> Absent]

VARIABLES
    committed,  \* [Terms -> AbsentOrVal] : the LIVE published value per term (the root)
    recov,      \* [Terms -> AbsentOrVal] : the value a crash-recover would reconstruct =
                \* the last RANKED durable write per term (Absent if none). The abstract
                \* image of "replay the ranked WAL records under the Overlay regime".
    pend        \* [Terms -> PendType] : the in-flight conditional write per term (the
                \* modeled racing writer — one in flight per term suffices for the race)

Vars == <<committed, recov, pend>>

PendType == [active: BOOLEAN, val: Vals, basis: AbsentOrVal]

TypeInvariant ==
    /\ committed \in [Terms -> AbsentOrVal]
    /\ recov \in [Terms -> AbsentOrVal]
    /\ pend \in [Terms -> PendType]

\* Empty overlay: nothing published, nothing recoverable, no in-flight write.
Init ==
    /\ committed = [t \in Terms |-> Absent]
    /\ recov = [t \in Terms |-> Absent]
    /\ pend = [t \in Terms |-> NonePend]

(***************************************************************************)
(* Actions.                                                                  *)
(***************************************************************************)

\* RecomputeAndAppend(t, v): a conditional writer reads `committed[t]` as its basis (the
\* CAS `expected`, or the value `merge_fn` recomputed against) and APPENDS + syncs the
\* durable `Upsert{v}` record BEFORE the publish CAS (Order-A step 1). One in-flight
\* write per term (the modeled racing writer). `committed`/`recov` unchanged: the record
\* is durable but not yet ranked, so it is invisible AND not yet recoverable.
RecomputeAndAppend(t, v) ==
    /\ ~pend[t].active
    /\ pend' = [pend EXCEPT ![t] = [active |-> TRUE, val |-> v, basis |-> committed[t]]]
    /\ UNCHANGED <<committed, recov>>

\* ConcurrentWin(t, v): a DIFFERENT writer wins the root CAS for t (its own
\* append+win+rank, abstracted as one atomic environment step). The live `committed`
\* and the recoverable `recov` both advance to `v` (a won, ranked write). This is the
\* "concurrent change" that makes an in-flight write's basis stale.
ConcurrentWin(t, v) ==
    /\ committed' = [committed EXCEPT ![t] = v]
    /\ recov' = [recov EXCEPT ![t] = v]
    /\ UNCHANGED pend

\* WinAndRank(t): the in-flight write WINS — `committed[t]` is unchanged since its append
\* (basis still matches), so the publish CAS lands. `committed` advances to the new
\* value and the record is RANKED (`commit_rank_and_mark`), so `recov` advances to the
\* SAME value (it is the highest-LSN ranked write for t). Ack `Ok(true)`. Models
\* `durable_write.rs:516` (`ValuePublishOutcome::Published` -> `commit_rank_and_mark`).
WinAndRank(t) ==
    /\ pend[t].active
    /\ pend[t].basis = committed[t]           \* no concurrent change -> the CAS lands
    /\ committed' = [committed EXCEPT ![t] = pend[t].val]
    /\ recov' = [recov EXCEPT ![t] = pend[t].val]
    /\ pend' = [pend EXCEPT ![t] = NonePend]

\* BurnOnLoss(t): the in-flight write LOSES — `committed[t]` changed since its append (a
\* concurrent winner), so the per-iteration `expected` recheck fails and the publish CAS
\* is REFUSED (`NotApplied`). The caller is told `Ok(false)`. `committed` is unchanged.
\* The already-durable `Upsert` record is BURNED:
\*  - DESIGN (USE_BURN_ON_LOSS): burned = UNRANKED. An Overlay-regime reopen DROPS it,
\*    so `recov` is UNCHANGED — the refused write never reconstructs. Models
\*    `mark_committed_burned` (`durable_write.rs:524-527`) + the regime drop
\*    (`recovery.rs:332`).
\*  - UNSAFE (~USE_BURN_ON_LOSS): the "forgot to burn" bug RANKS the refused record. It
\*    was appended after the winner, so it is the highest-LSN ranked write for t and
\*    `recov` advances to its value -> a PHANTOM (a value the caller was told Ok(false)
\*    that a reopen would resurrect, while the live `committed` keeps the winner).
BurnOnLoss(t) ==
    /\ pend[t].active
    /\ pend[t].basis # committed[t]           \* concurrent change -> the CAS is refused
    /\ committed' = committed
    /\ recov' = IF USE_BURN_ON_LOSS
                THEN recov                                  \* unranked -> dropped on reopen
                ELSE [recov EXCEPT ![t] = pend[t].val]      \* BUG: ranked -> phantom
    /\ pend' = [pend EXCEPT ![t] = NonePend]

Next ==
    \/ \E t \in Terms, v \in Vals : RecomputeAndAppend(t, v)
    \/ \E t \in Terms, v \in Vals : ConcurrentWin(t, v)
    \/ \E t \in Terms : WinAndRank(t)
    \/ \E t \in Terms : BurnOnLoss(t)

Spec == Init /\ [][Next]_Vars

------------------------------------------------------------------------------
\* Invariants. All vars range over finite domains (`AbsentOrVal` and the finite
\* `PendType` over the finite `Terms`), so the state space is finite — no monotone
\* counter and hence no TLC CONSTRAINT is needed (unlike the root-versioned models).

\* (1) NoPhantomConditionalWrite — the headline obligation. A crash-recover (`recov`,
\* the last RANKED durable write per term) reconstructs EXACTLY the live published state
\* (`committed`). No conditional write the caller was told `Ok(false)` (a burned record)
\* appears after recovery. Under the design every burned record is unranked and dropped,
\* so `recov = committed` is preserved by every action. The unsafe control ranks a
\* refused record, diverging `recov` from `committed` -> TLC must catch it.
NoPhantomConditionalWrite == \A t \in Terms : recov[t] = committed[t]

\* (2) NoLostConditionalWrite — a WON conditional write is never silently dropped on
\* recover: if a term is live-present (some write won), recover reconstructs a value for
\* it (not Absent). Implied by (1) under the design; stated as the dual obligation (no
\* won write vanishes), mirroring `LockFreeOverlayRemoveCas`'s `NoLostOp`.
NoLostConditionalWrite == \A t \in Terms : (committed[t] # Absent) => (recov[t] # Absent)

==============================================================================
