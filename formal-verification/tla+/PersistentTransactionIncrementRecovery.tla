---------------- MODULE PersistentTransactionIncrementRecovery ----------------
(***************************************************************************)
(* Focused model for checked document-transaction increments and             *)
(* BatchIncrement recovery.                                                  *)
(*                                                                         *)
(* Obligations captured here:                                                *)
(*   1. transaction delta aggregation is checked before commit publication;   *)
(*   2. current-value overflow rejects commit without appending WAL;          *)
(*   3. committed BatchIncrement records carry only checked arithmetic; and   *)
(*   4. replay stops at an invalid arithmetic record instead of applying the  *)
(*      invalid record or any suffix.                                        *)
(***************************************************************************)

EXTENDS Integers, Naturals, Sequences, TLC

CONSTANTS
    MaxKeys,
    MaxCounter,
    MaxDelta,
    MaxWal

ASSUME MaxKeys \in Nat \ {0}
ASSUME MaxCounter \in Nat \ {0}
ASSUME MaxDelta \in Nat \ {0}
ASSUME MaxWal \in Nat \ {0}

Keys == 1..MaxKeys
CounterValues == (-MaxCounter)..MaxCounter
Deltas == (-MaxDelta)..MaxDelta

ZeroValues == [k \in Keys |-> 0]
ZeroDeltas == [k \in Keys |-> 0]

CheckedAdd(current, delta) ==
    current + delta \in CounterValues

CheckedBatch(vals, deltas) ==
    \A k \in Keys : CheckedAdd(vals[k], deltas[k])

ApplyBatch(vals, deltas) ==
    [k \in Keys |-> vals[k] + deltas[k]]

WalRecord ==
    [source : {"commit", "external"},
     delta : [Keys -> Deltas],
     before : [Keys -> CounterValues],
     after : [Keys -> CounterValues]]

VARIABLES
    values,
    wal,
    txActive,
    txFailed,
    txDelta,
    failureCut,
    replayValues,
    replayIndex,
    replayStopped

vars == <<values, wal, txActive, txFailed, txDelta, failureCut,
          replayValues, replayIndex, replayStopped>>

Init ==
    /\ values = ZeroValues
    /\ wal = <<>>
    /\ txActive = FALSE
    /\ txFailed = FALSE
    /\ txDelta = ZeroDeltas
    /\ failureCut = 0
    /\ replayValues = ZeroValues
    /\ replayIndex = 1
    /\ replayStopped = FALSE

BeginTx ==
    /\ ~txActive
    /\ ~txFailed
    /\ txActive' = TRUE
    /\ txDelta' = ZeroDeltas
    /\ UNCHANGED <<values, wal, txFailed, failureCut,
                   replayValues, replayIndex, replayStopped>>

StageIncrementOk(key, delta) ==
    /\ txActive
    /\ ~txFailed
    /\ key \in Keys
    /\ delta \in Deltas
    /\ txDelta[key] + delta \in Deltas
    /\ txDelta' = [txDelta EXCEPT ![key] = @ + delta]
    /\ UNCHANGED <<values, wal, txActive, txFailed, failureCut,
                   replayValues, replayIndex, replayStopped>>

StageIncrementOverflow(key, delta) ==
    /\ txActive
    /\ ~txFailed
    /\ key \in Keys
    /\ delta \in Deltas
    /\ txDelta[key] + delta \notin Deltas
    /\ txFailed' = TRUE
    /\ failureCut' = Len(wal)
    /\ UNCHANGED <<values, wal, txActive, txDelta,
                   replayValues, replayIndex, replayStopped>>

CommitOk ==
    /\ txActive
    /\ ~txFailed
    /\ Len(wal) < MaxWal
    /\ CheckedBatch(values, txDelta)
    /\ wal' = Append(wal,
        [source |-> "commit",
         delta |-> txDelta,
         before |-> values,
         after |-> ApplyBatch(values, txDelta)])
    /\ values' = ApplyBatch(values, txDelta)
    /\ txActive' = FALSE
    /\ txDelta' = ZeroDeltas
    /\ UNCHANGED <<txFailed, failureCut, replayValues, replayIndex,
                   replayStopped>>

CommitOverflow ==
    /\ txActive
    /\ ~txFailed
    /\ ~CheckedBatch(values, txDelta)
    /\ txFailed' = TRUE
    /\ failureCut' = Len(wal)
    /\ UNCHANGED <<values, wal, txActive, txDelta,
                   replayValues, replayIndex, replayStopped>>

\* Models an old/corrupt but syntactically valid WAL suffix.  The production
\* commit path cannot create this source, but recovery must still fail closed.
InjectInvalidWalRecord(deltas) ==
    /\ Len(wal) < MaxWal
    /\ ~txFailed
    /\ deltas \in [Keys -> Deltas]
    /\ ~CheckedBatch(values, deltas)
    /\ wal' = Append(wal,
        [source |-> "external",
         delta |-> deltas,
         before |-> values,
         after |-> values])
    /\ UNCHANGED <<values, txActive, txFailed, txDelta, failureCut,
                   replayValues, replayIndex, replayStopped>>

ReplayNextOk ==
    /\ ~replayStopped
    /\ replayIndex \in 1..Len(wal)
    /\ CheckedBatch(replayValues, wal[replayIndex].delta)
    /\ replayValues' = ApplyBatch(replayValues, wal[replayIndex].delta)
    /\ replayIndex' = replayIndex + 1
    /\ UNCHANGED <<values, wal, txActive, txFailed, txDelta, failureCut,
                   replayStopped>>

ReplayNextOverflow ==
    /\ ~replayStopped
    /\ replayIndex \in 1..Len(wal)
    /\ ~CheckedBatch(replayValues, wal[replayIndex].delta)
    /\ replayStopped' = TRUE
    /\ UNCHANGED <<values, wal, txActive, txFailed, txDelta, failureCut,
                   replayValues, replayIndex>>

ReplayDone ==
    /\ replayIndex = Len(wal) + 1
    /\ UNCHANGED vars

Next ==
    \/ BeginTx
    \/ \E key \in Keys, delta \in Deltas : StageIncrementOk(key, delta)
    \/ \E key \in Keys, delta \in Deltas : StageIncrementOverflow(key, delta)
    \/ CommitOk
    \/ CommitOverflow
    \/ \E deltas \in [Keys -> Deltas] : InjectInvalidWalRecord(deltas)
    \/ ReplayNextOk
    \/ ReplayNextOverflow
    \/ ReplayDone

TypeInvariant ==
    /\ values \in [Keys -> CounterValues]
    /\ wal \in Seq(WalRecord)
    /\ Len(wal) <= MaxWal
    /\ txActive \in BOOLEAN
    /\ txFailed \in BOOLEAN
    /\ txDelta \in [Keys -> Deltas]
    /\ failureCut \in 0..MaxWal
    /\ replayValues \in [Keys -> CounterValues]
    /\ replayIndex \in 1..(Len(wal) + 1)
    /\ replayStopped \in BOOLEAN

CommittedWalRecordsAreChecked ==
    \A i \in DOMAIN wal :
        wal[i].source = "commit" =>
            /\ CheckedBatch(wal[i].before, wal[i].delta)
            /\ wal[i].after = ApplyBatch(wal[i].before, wal[i].delta)

FailedTransactionDoesNotAppendWal ==
    txFailed => Len(wal) = failureCut

ReplayStopsAtInvalidArithmetic ==
    replayStopped => replayIndex \in 1..Len(wal)

Spec == Init /\ [][Next]_vars

=============================================================================
