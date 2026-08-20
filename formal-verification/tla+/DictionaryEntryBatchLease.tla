----------------------- MODULE DictionaryEntryBatchLease -----------------------
(***************************************************************************)
(* The move-only batch-lease protocol of vt.dict.entry.v1.                *)
(*                                                                         *)
(* A cursor owns one immutable dictionary revision. next_batch may expose  *)
(* exactly one borrowed arena at a time; release_batch must present that   *)
(* arena's exact generation. LimitExceeded is retryable, End is sticky,    *)
(* cancel does not invalidate an outstanding borrow, and close refuses to  *)
(* destroy a cursor while either a caller or reducer callback can see its  *)
(* arena. reduce uses the same lease state internally and must settle it   *)
(* before returning control to its caller.                                 *)
(*                                                                         *)
(* Registry correspondence (formal-verification/ABI_INVARIANTS.tsv):      *)
(*   LDICT-ENTRY-1 LeaseGenerationExact                                    *)
(*   LDICT-ENTRY-2 RetryPreservesCursor                                    *)
(*   LDICT-ENTRY-3 LiveLeaseNeverAliased                                   *)
(*   LDICT-ENTRY-4 InvalidReleasePreservesLease                            *)
(*   LDICT-ENTRY-5 EndIsSticky                                             *)
(*   LDICT-ENTRY-6 CancelPreservesLiveLease                                *)
(*   LDICT-ENTRY-7 CloseRefusalPreservesOwnership                          *)
(*   LDICT-ENTRY-8 ReducerAutoSettles                                      *)
(*                                                                         *)
(* TLC proves this finite instance. The upgrade rung is an unbounded       *)
(* refinement proof over the same four-state lease machine.                *)
(***************************************************************************)
EXTENDS Integers, Naturals, TLC

CONSTANTS EntryCount, MaxBatch, MaxGeneration

ASSUME EntryCount \in Nat
ASSUME MaxBatch \in Nat \ {0}
ASSUME MaxGeneration \in Nat \ {0}

Phases == {"Idle", "Leased", "Reducing", "Ended", "Closed"}
Statuses == {"None", "Ok", "End", "LimitExceeded", "BatchInUse",
             "InvalidArgument"}
Operations == {"Init", "NextOk", "NextLimit", "NextBlocked", "NextEnd",
               "NextStickyEnd", "ReleaseOk", "ReleaseBad", "Cancel",
               "CloseOk", "CloseBlocked", "ReduceStart", "ReduceReentry",
               "ReduceSettle"}

VARIABLES
  phase, position, generation, leaseGeneration, cancelled, internalBatch,
  reducedCount, lastOp, lastStatus,
  priorPhase, priorPosition, priorGeneration, priorLeaseGeneration

vars == <<phase, position, generation, leaseGeneration, cancelled,
          internalBatch, reducedCount, lastOp, lastStatus,
          priorPhase, priorPosition, priorGeneration, priorLeaseGeneration>>

Primary == <<phase, position, generation, leaseGeneration, cancelled,
             internalBatch, reducedCount>>

Observe(op, status) ==
  /\ lastOp' = op
  /\ lastStatus' = status
  /\ priorPhase' = phase
  /\ priorPosition' = position
  /\ priorGeneration' = generation
  /\ priorLeaseGeneration' = leaseGeneration

Init ==
  /\ phase = "Idle"
  /\ position = 0
  /\ generation = 0
  /\ leaseGeneration = 0
  /\ cancelled = FALSE
  /\ internalBatch = 0
  /\ reducedCount = 0
  /\ lastOp = "Init"
  /\ lastStatus = "None"
  /\ priorPhase = "Idle"
  /\ priorPosition = 0
  /\ priorGeneration = 0
  /\ priorLeaseGeneration = 0

NextOk(n) ==
  /\ phase = "Idle"
  /\ ~cancelled
  /\ position < EntryCount
  /\ generation < MaxGeneration
  /\ n \in 1..(IF MaxBatch < EntryCount - position
                THEN MaxBatch ELSE EntryCount - position)
  /\ phase' = "Leased"
  /\ position' = position + n
  /\ generation' = generation + 1
  /\ leaseGeneration' = generation + 1
  /\ internalBatch' = 0
  /\ UNCHANGED <<cancelled, reducedCount>>
  /\ Observe("NextOk", "Ok")

(* The caller's limits cannot hold the first pending entry, or the finite  *)
(* generation space is exhausted. Both cases are non-consuming retries.    *)
NextLimit ==
  /\ phase = "Idle"
  /\ ~cancelled
  /\ position < EntryCount
  /\ UNCHANGED Primary
  /\ Observe("NextLimit", "LimitExceeded")

NextBlocked ==
  /\ phase \in {"Leased", "Reducing"}
  /\ UNCHANGED Primary
  /\ Observe("NextBlocked", "BatchInUse")

NextEnd ==
  /\ phase = "Idle"
  /\ (cancelled \/ position = EntryCount)
  /\ phase' = "Ended"
  /\ leaseGeneration' = 0
  /\ internalBatch' = 0
  /\ UNCHANGED <<position, generation, cancelled, reducedCount>>
  /\ Observe("NextEnd", "End")

NextStickyEnd ==
  /\ phase = "Ended"
  /\ UNCHANGED Primary
  /\ Observe("NextStickyEnd", "End")

ReleaseOk ==
  /\ phase = "Leased"
  /\ leaseGeneration > 0
  /\ phase' = "Idle"
  /\ leaseGeneration' = 0
  /\ UNCHANGED <<position, generation, cancelled, internalBatch, reducedCount>>
  /\ Observe("ReleaseOk", "Ok")

(* Represents every stale, duplicate, zero, or otherwise non-current       *)
(* generation supplied to release_batch.                                   *)
ReleaseBad ==
  /\ phase # "Closed"
  /\ UNCHANGED Primary
  /\ Observe("ReleaseBad", "InvalidArgument")

Cancel ==
  /\ phase # "Closed"
  /\ cancelled' = TRUE
  /\ UNCHANGED <<phase, position, generation, leaseGeneration,
                 internalBatch, reducedCount>>
  /\ Observe("Cancel", "Ok")

CloseOk ==
  /\ phase \in {"Idle", "Ended"}
  /\ phase' = "Closed"
  /\ leaseGeneration' = 0
  /\ internalBatch' = 0
  /\ UNCHANGED <<position, generation, cancelled, reducedCount>>
  /\ Observe("CloseOk", "Ok")

CloseBlocked ==
  /\ phase \in {"Leased", "Reducing"}
  /\ UNCHANGED Primary
  /\ Observe("CloseBlocked", "BatchInUse")

(* reduce obtains the same arena lease before invoking user code. The      *)
(* callback can attempt reentry, but observes BatchInUse.                   *)
ReduceStart(n) ==
  /\ phase = "Idle"
  /\ ~cancelled
  /\ position < EntryCount
  /\ generation < MaxGeneration
  /\ n \in 1..(IF MaxBatch < EntryCount - position
                THEN MaxBatch ELSE EntryCount - position)
  /\ phase' = "Reducing"
  /\ position' = position + n
  /\ generation' = generation + 1
  /\ leaseGeneration' = generation + 1
  /\ internalBatch' = n
  /\ UNCHANGED <<cancelled, reducedCount>>
  /\ Observe("ReduceStart", "Ok")

ReduceReentry ==
  /\ phase = "Reducing"
  /\ UNCHANGED Primary
  /\ Observe("ReduceReentry", "BatchInUse")

ReduceSettle ==
  /\ phase = "Reducing"
  /\ phase' = IF cancelled \/ position = EntryCount THEN "Ended" ELSE "Idle"
  /\ leaseGeneration' = 0
  /\ reducedCount' = reducedCount + internalBatch
  /\ internalBatch' = 0
  /\ UNCHANGED <<position, generation, cancelled>>
  /\ Observe("ReduceSettle", "Ok")

Next ==
  \/ \E n \in 1..MaxBatch : NextOk(n)
  \/ NextLimit
  \/ NextBlocked
  \/ NextEnd
  \/ NextStickyEnd
  \/ ReleaseOk
  \/ ReleaseBad
  \/ Cancel
  \/ CloseOk
  \/ CloseBlocked
  \/ \E n \in 1..MaxBatch : ReduceStart(n)
  \/ ReduceReentry
  \/ ReduceSettle

Spec == Init /\ [][Next]_vars /\ WF_vars(ReduceSettle)

TypeOK ==
  /\ phase \in Phases
  /\ position \in 0..EntryCount
  /\ generation \in 0..MaxGeneration
  /\ leaseGeneration \in 0..MaxGeneration
  /\ cancelled \in BOOLEAN
  /\ internalBatch \in 0..MaxBatch
  /\ reducedCount \in Nat
  /\ lastOp \in Operations
  /\ lastStatus \in Statuses
  /\ priorPhase \in Phases
  /\ priorPosition \in 0..EntryCount
  /\ priorGeneration \in 0..MaxGeneration
  /\ priorLeaseGeneration \in 0..MaxGeneration

(* LDICT-ENTRY-1: every exposed arena carries the freshly incremented,      *)
(* nonzero generation and no generation can wrap.                          *)
LeaseGenerationExact ==
  /\ phase \in {"Leased", "Reducing"} => leaseGeneration = generation
  /\ phase \notin {"Leased", "Reducing"} => leaseGeneration = 0

(* LDICT-ENTRY-2: a too-small batch request consumes nothing.               *)
RetryPreservesCursor ==
  lastOp = "NextLimit" =>
    /\ position = priorPosition
    /\ generation = priorGeneration
    /\ leaseGeneration = priorLeaseGeneration
    /\ phase = priorPhase

(* LDICT-ENTRY-3: while an arena is visible, no second request can replace  *)
(* it or advance the cursor.                                                *)
LiveLeaseNeverAliased ==
  lastOp \in {"NextBlocked", "ReduceReentry"} =>
    /\ lastStatus = "BatchInUse"
    /\ position = priorPosition
    /\ generation = priorGeneration
    /\ leaseGeneration = priorLeaseGeneration
    /\ phase = priorPhase

(* LDICT-ENTRY-4: rejected stale/double releases cannot settle or corrupt a *)
(* current cursor.                                                          *)
InvalidReleasePreservesLease ==
  lastOp = "ReleaseBad" =>
    /\ position = priorPosition
    /\ generation = priorGeneration
    /\ leaseGeneration = priorLeaseGeneration
    /\ phase = priorPhase

(* LDICT-ENTRY-5: End returns an empty, generation-zero view forever.        *)
EndIsSticky ==
  lastOp = "NextStickyEnd" =>
    /\ phase = "Ended"
    /\ lastStatus = "End"
    /\ leaseGeneration = 0
    /\ position = priorPosition
    /\ generation = priorGeneration

(* LDICT-ENTRY-6: cancel is idempotent and cannot invalidate memory already *)
(* borrowed by the caller.                                                  *)
CancelPreservesLiveLease ==
  lastOp = "Cancel" /\ priorPhase \in {"Leased", "Reducing"} =>
    /\ phase = priorPhase
    /\ leaseGeneration = priorLeaseGeneration
    /\ generation = priorGeneration

(* LDICT-ENTRY-7: a failed close leaves ownership with the caller.          *)
CloseRefusalPreservesOwnership ==
  lastOp = "CloseBlocked" =>
    /\ phase = priorPhase
    /\ leaseGeneration = priorLeaseGeneration
    /\ generation = priorGeneration
    /\ position = priorPosition

(* LDICT-ENTRY-8: every completed reducer callback settles its internal     *)
(* lease before the public call can return.                                 *)
ReducerAutoSettles ==
  lastOp = "ReduceSettle" =>
    /\ phase \in {"Idle", "Ended"}
    /\ leaseGeneration = 0
    /\ internalBatch = 0

ReducerEventuallySettles == phase = "Reducing" ~> phase # "Reducing"

=============================================================================
