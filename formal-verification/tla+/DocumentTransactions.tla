-------------------------- MODULE DocumentTransactions --------------------------
(****************************************************************************)
(* Bounded document transaction model.                                      *)
(*                                                                          *)
(* Scope: begin/stage/commit/abort behavior for one-key document writes.    *)
(* The model checks that only committed staged writes become visible and     *)
(* that commit/abort WAL records are scoped to transactions that began.     *)
(****************************************************************************)

EXTENDS Naturals, FiniteSets, TLC

CONSTANTS Keys, Values, TxIds, Null

VARIABLES store, txState, txWriteKey, txWriteValue, wal

Vars == <<store, txState, txWriteKey, txWriteValue, wal>>

TxStates == {"Idle", "Open", "Committed", "Aborted"}
WalKinds == {"Begin", "Write", "Commit", "Abort"}

WalRecordSet ==
    [tx : TxIds, kind : WalKinds, key : Keys \cup {Null}, value : Values \cup {Null}]

TypeInvariant ==
    /\ store \in [Keys -> Values \cup {Null}]
    /\ txState \in [TxIds -> TxStates]
    /\ txWriteKey \in [TxIds -> Keys \cup {Null}]
    /\ txWriteValue \in [TxIds -> Values \cup {Null}]
    /\ wal \in SUBSET WalRecordSet

Init ==
    /\ store = [k \in Keys |-> Null]
    /\ txState = [tx \in TxIds |-> "Idle"]
    /\ txWriteKey = [tx \in TxIds |-> Null]
    /\ txWriteValue = [tx \in TxIds |-> Null]
    /\ wal = {}

Begin(tx) ==
    /\ tx \in TxIds
    /\ txState[tx] = "Idle"
    /\ txState' = [txState EXCEPT ![tx] = "Open"]
    /\ txWriteKey' = txWriteKey
    /\ txWriteValue' = txWriteValue
    /\ wal' = wal \cup {[tx |-> tx, kind |-> "Begin", key |-> Null, value |-> Null]}
    /\ UNCHANGED store

StageWrite(tx, k, v) ==
    /\ tx \in TxIds
    /\ k \in Keys
    /\ v \in Values
    /\ txState[tx] = "Open"
    /\ txWriteKey' = [txWriteKey EXCEPT ![tx] = k]
    /\ txWriteValue' = [txWriteValue EXCEPT ![tx] = v]
    /\ wal' = wal \cup {[tx |-> tx, kind |-> "Write", key |-> k, value |-> v]}
    /\ UNCHANGED <<store, txState>>

Commit(tx) ==
    /\ tx \in TxIds
    /\ txState[tx] = "Open"
    /\ txWriteKey[tx] # Null
    /\ store' = [store EXCEPT ![txWriteKey[tx]] = txWriteValue[tx]]
    /\ txState' = [txState EXCEPT ![tx] = "Committed"]
    /\ wal' = wal \cup {[tx |-> tx, kind |-> "Commit", key |-> Null, value |-> Null]}
    /\ UNCHANGED <<txWriteKey, txWriteValue>>

Abort(tx) ==
    /\ tx \in TxIds
    /\ txState[tx] = "Open"
    /\ txState' = [txState EXCEPT ![tx] = "Aborted"]
    /\ wal' = wal \cup {[tx |-> tx, kind |-> "Abort", key |-> Null, value |-> Null]}
    /\ UNCHANGED <<store, txWriteKey, txWriteValue>>

Next ==
    \/ \E tx \in TxIds : Begin(tx)
    \/ \E tx \in TxIds, k \in Keys, v \in Values : StageWrite(tx, k, v)
    \/ \E tx \in TxIds : Commit(tx)
    \/ \E tx \in TxIds : Abort(tx)

VisibleValuesComeFromCommittedWrites ==
    \A k \in Keys :
        store[k] # Null =>
            \E tx \in TxIds :
                /\ txState[tx] = "Committed"
                /\ txWriteKey[tx] = k
                /\ txWriteValue[tx] = store[k]

CommitHasBeginRecord ==
    \A r \in wal :
        r.kind = "Commit" =>
            \E b \in wal : b.tx = r.tx /\ b.kind = "Begin"

AbortHasBeginRecord ==
    \A r \in wal :
        r.kind = "Abort" =>
            \E b \in wal : b.tx = r.tx /\ b.kind = "Begin"

NoCommitWithoutStagedWrite ==
    \A tx \in TxIds :
        txState[tx] = "Committed" => txWriteKey[tx] # Null

Spec == Init /\ [][Next]_Vars

=============================================================================
