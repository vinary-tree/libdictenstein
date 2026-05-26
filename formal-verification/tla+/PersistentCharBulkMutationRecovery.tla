---------------- MODULE PersistentCharBulkMutationRecovery ----------------
(***************************************************************************)
(* Focused model for PersistentARTrieChar bulk mutation recovery.           *)
(*                                                                         *)
(* The model captures two implementation obligations:                       *)
(*   1. remove_prefix_batched collects a prefix plan, then writes one WAL    *)
(*      remove before each visible in-memory removal.  Crash recovery sees  *)
(*      exactly the durable prefix of remove records.                       *)
(*   2. checked numeric increment writes an Increment record only when the   *)
(*      i64-range addition succeeds; overflow is a fail-closed stutter.     *)
(***************************************************************************)

EXTENDS Integers, Naturals, Sequences, FiniteSets, TLC

CONSTANTS
    MaxKeys,
    PrefixKeyCount,
    MaxCounter,
    MaxDelta,
    MaxIncrements

ASSUME MaxKeys \in Nat \ {0}
ASSUME PrefixKeyCount \in 0..MaxKeys
ASSUME MaxCounter \in Nat \ {0}
ASSUME MaxDelta \in Nat \ {0}
ASSUME MaxIncrements \in Nat

Keys == 1..MaxKeys
NoKey == MaxKeys + 1
MaybeKey == Keys \cup {NoKey}
PrefixKeys == 1..PrefixKeyCount
CounterValues == (-MaxCounter)..MaxCounter
Deltas == (-MaxDelta)..MaxDelta
KeySeq == [i \in Keys |-> i]

IncrementRecord ==
    [key : Keys, before : CounterValues, delta : Deltas, result : CounterValues]

VARIABLES
    \* Keys currently visible in memory.
    present,

    \* Durable remove records written by remove_prefix_batched.
    walDeletes,

    \* Current collected deletion plan.
    deletePlan,

    \* A delete that is durable in WAL but not yet applied in memory.
    pendingDelete,

    \* Prefix deletion control state.
    phase,

    \* Numeric values for checked read-modify-write operations.
    values,

    \* Durable successful increment records.
    walIncrements

vars == <<present, walDeletes, deletePlan, pendingDelete, phase, values,
          walIncrements>>

SeqToSet(s) == {s[i] : i \in DOMAIN s}

NoDuplicateSeq(s) ==
    \A i, j \in DOMAIN s : i # j => s[i] # s[j]

PendingSet ==
    IF pendingDelete = NoKey THEN {} ELSE {pendingDelete}

RecoverPresent ==
    Keys \ SeqToSet(walDeletes)

CheckedAdd(current, delta) ==
    current + delta \in CounterValues

Init ==
    /\ present = Keys
    /\ walDeletes = <<>>
    /\ deletePlan = <<>>
    /\ pendingDelete = NoKey
    /\ phase = "Ready"
    /\ values = [k \in Keys |-> 0]
    /\ walIncrements = <<>>

\* Collection may fail while resolving lazy children.  The implementation must
\* return an error before appending WAL or mutating memory.
CollectFailure ==
    /\ phase = "Ready"
    /\ UNCHANGED vars

CollectPrefixDeletes ==
    /\ phase = "Ready"
    /\ deletePlan' =
        SelectSeq(KeySeq, LAMBDA k : k \in present /\ k \in PrefixKeys)
    /\ phase' = "Removing"
    /\ UNCHANGED <<present, walDeletes, pendingDelete, values, walIncrements>>

FinishPrefixDeletes ==
    /\ phase = "Removing"
    /\ deletePlan = <<>>
    /\ pendingDelete = NoKey
    /\ phase' = "Ready"
    /\ UNCHANGED <<present, walDeletes, deletePlan, pendingDelete, values,
                   walIncrements>>

\* WAL is appended before the corresponding key disappears from memory.
AppendNextDelete ==
    /\ phase = "Removing"
    /\ pendingDelete = NoKey
    /\ Len(deletePlan) > 0
    /\ LET key == Head(deletePlan) IN
        /\ walDeletes' = Append(walDeletes, key)
        /\ pendingDelete' = key
        /\ UNCHANGED <<present, deletePlan, phase, values, walIncrements>>

ApplyPendingDelete ==
    /\ pendingDelete # NoKey
    /\ present' = present \ {pendingDelete}
    /\ deletePlan' = Tail(deletePlan)
    /\ pendingDelete' = NoKey
    /\ UNCHANGED <<walDeletes, phase, values, walIncrements>>

IncrementOk(key, delta) ==
    /\ key \in Keys
    /\ delta \in Deltas
    /\ Len(walIncrements) < MaxIncrements
    /\ CheckedAdd(values[key], delta)
    /\ LET result == values[key] + delta IN
        /\ walIncrements' =
            Append(walIncrements,
                [key |-> key,
                 before |-> values[key],
                 delta |-> delta,
                 result |-> result])
        /\ values' = [values EXCEPT ![key] = result]
        /\ UNCHANGED <<present, walDeletes, deletePlan, pendingDelete, phase>>

\* Overflow is intentionally a stutter: no WAL record and no memory update.
IncrementOverflow(key, delta) ==
    /\ key \in Keys
    /\ delta \in Deltas
    /\ ~CheckedAdd(values[key], delta)
    /\ UNCHANGED vars

Next ==
    \/ CollectFailure
    \/ CollectPrefixDeletes
    \/ FinishPrefixDeletes
    \/ AppendNextDelete
    \/ ApplyPendingDelete
    \/ \E key \in Keys, delta \in Deltas : IncrementOk(key, delta)
    \/ \E key \in Keys, delta \in Deltas : IncrementOverflow(key, delta)

TypeInvariant ==
    /\ present \subseteq Keys
    /\ walDeletes \in Seq(Keys)
    /\ deletePlan \in Seq(Keys)
    /\ pendingDelete \in MaybeKey
    /\ phase \in {"Ready", "Removing"}
    /\ values \in [Keys -> CounterValues]
    /\ walIncrements \in Seq(IncrementRecord)
    /\ phase = "Ready" => deletePlan = <<>>
    /\ pendingDelete # NoKey => pendingDelete \in PrefixKeys

\* A key cannot disappear from the visible map before its remove record exists.
VisibleDeleteHasWal ==
    Keys \ present \subseteq SeqToSet(walDeletes)

\* After a WAL append and before memory mutation, recovery may be one delete
\* ahead of memory.  No other divergence is permitted.
RecoveryIsDurablePrefix ==
    present \ PendingSet = RecoverPresent

\* remove_prefix_batched must not write duplicate remove records.
DeleteRecordsAreUnique ==
    NoDuplicateSeq(walDeletes)

DeleteRecordsStayInPrefix ==
    SeqToSet(walDeletes) \subseteq PrefixKeys

\* Successful increment records are exactly the checked arithmetic cases.
IncrementRecordsAreChecked ==
    \A i \in DOMAIN walIncrements :
        /\ walIncrements[i].result \in CounterValues
        /\ walIncrements[i].result =
           walIncrements[i].before + walIncrements[i].delta

Spec == Init /\ [][Next]_vars

=============================================================================
