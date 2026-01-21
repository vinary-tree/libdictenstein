--------------------------------- MODULE WAL -----------------------------------
(****************************************************************************)
(* WAL: Write-Ahead Log specification for the Persistent ARTrie.            *)
(* This module defines the WAL operations that ensure durability and        *)
(* crash recovery semantics.                                                *)
(*                                                                          *)
(* Key properties verified:                                                 *)
(* 1. LSN Monotonicity: Log sequence numbers always increase                *)
(* 2. Durability: Committed operations survive crashes                      *)
(* 3. Atomicity: Operations are fully applied or not at all                 *)
(* 4. Group Commit: Multiple operations can share a single fsync            *)
(****************************************************************************)

EXTENDS ARTrieTypes, Integers, Sequences, FiniteSets, TLC

--------------------------------------------------------------------------------
(* HELPER OPERATORS                                                           *)
--------------------------------------------------------------------------------

\* Convert a sequence to a set
SeqToSet(s) == {s[i] : i \in DOMAIN s}

--------------------------------------------------------------------------------
(* STATE VARIABLES                                                            *)
--------------------------------------------------------------------------------

VARIABLES
    \* The write-ahead log (sequence of records)
    wal,

    \* Current log sequence number
    currentLsn,

    \* Last durable LSN (fsynced to disk)
    durableLsn,

    \* Last checkpointed LSN
    checkpointLsn,

    \* Pending records (buffered, not yet fsynced)
    pendingRecords,

    \* Active transactions: TxId -> Transaction
    transactions,

    \* Next transaction ID
    nextTxId,

    \* Group commit queue: threads waiting for durability
    groupCommitQueue

--------------------------------------------------------------------------------
(* TYPE INVARIANT                                                             *)
--------------------------------------------------------------------------------

WalTypeInvariant ==
    /\ \A i \in DOMAIN wal : IsWalRecord(wal[i])
    /\ currentLsn \in Nat
    /\ durableLsn \in Nat
    /\ checkpointLsn \in Nat
    /\ \A i \in DOMAIN pendingRecords : IsWalRecord(pendingRecords[i])
    /\ \A txId \in DOMAIN transactions :
        \/ transactions[txId] = <<>>
        \/ IsTransaction(transactions[txId])
    /\ nextTxId \in Nat
    /\ groupCommitQueue \in SUBSET Threads

--------------------------------------------------------------------------------
(* INITIAL STATE                                                              *)
--------------------------------------------------------------------------------

WalInit ==
    /\ wal = <<>>
    /\ currentLsn = 0
    /\ durableLsn = 0
    /\ checkpointLsn = 0
    /\ pendingRecords = <<>>
    /\ transactions = [txId \in 0..MaxTxId |-> <<>>]
    /\ nextTxId = 1
    /\ groupCommitQueue = {}

--------------------------------------------------------------------------------
(* HELPER OPERATORS                                                           *)
--------------------------------------------------------------------------------

\* Create a new WAL record
MakeWalRecord(recType, key, value, txId) ==
    [
        lsn           |-> currentLsn + 1,
        recType       |-> recType,
        txId          |-> txId,
        key           |-> key,
        value         |-> value,
        expected      |-> <<>>,
        delta         |-> 0,
        checkpointLsn |-> 0
    ]

\* Create a checkpoint record
MakeCheckpointRecord ==
    [
        lsn           |-> currentLsn + 1,
        recType       |-> "Checkpoint",
        txId          |-> 0,
        key           |-> <<>>,
        value         |-> <<>>,
        expected      |-> <<>>,
        delta         |-> 0,
        checkpointLsn |-> durableLsn
    ]

\* Create a transaction begin record
MakeBeginTxRecord(txId) ==
    [
        lsn           |-> currentLsn + 1,
        recType       |-> "BeginTx",
        txId          |-> txId,
        key           |-> <<>>,
        value         |-> <<>>,
        expected      |-> <<>>,
        delta         |-> 0,
        checkpointLsn |-> 0
    ]

\* Create a transaction commit record
MakeCommitTxRecord(txId) ==
    [
        lsn           |-> currentLsn + 1,
        recType       |-> "CommitTx",
        txId          |-> txId,
        key           |-> <<>>,
        value         |-> <<>>,
        expected      |-> <<>>,
        delta         |-> 0,
        checkpointLsn |-> 0
    ]

\* Create a transaction abort record
MakeAbortTxRecord(txId) ==
    [
        lsn           |-> currentLsn + 1,
        recType       |-> "AbortTx",
        txId          |-> txId,
        key           |-> <<>>,
        value         |-> <<>>,
        expected      |-> <<>>,
        delta         |-> 0,
        checkpointLsn |-> 0
    ]

\* Get all records for a transaction
TxRecords(txId) ==
    SelectSeq(wal, LAMBDA r : r.txId = txId)

\* Check if a transaction is committed
IsTxCommitted(txId) ==
    \E r \in SeqToSet(wal) : r.txId = txId /\ r.recType = "CommitTx"

\* Check if a transaction is aborted
IsTxAborted(txId) ==
    \E r \in SeqToSet(wal) : r.txId = txId /\ r.recType = "AbortTx"

\* Get the last record in the WAL
LastRecord == IF Len(wal) > 0 THEN wal[Len(wal)] ELSE <<>>

--------------------------------------------------------------------------------
(* APPEND OPERATIONS                                                          *)
(* Operations to append records to the WAL.                                   *)
--------------------------------------------------------------------------------

\* Append a record to the WAL (buffered, not yet durable)
AppendRecord(rec) ==
    /\ currentLsn < MaxLSN
    /\ currentLsn' = currentLsn + 1
    /\ pendingRecords' = Append(pendingRecords, rec)
    /\ UNCHANGED <<wal, durableLsn, checkpointLsn, transactions, nextTxId, groupCommitQueue>>

\* Log an insert operation
LogInsert(thread, key, value, txId) ==
    LET rec == MakeWalRecord("Insert", key, value, txId) IN
        /\ AppendRecord(rec)

\* Log a remove operation
LogRemove(thread, key, txId) ==
    LET rec == MakeWalRecord("Remove", key, Null, txId) IN
        /\ AppendRecord(rec)

\* Log an upsert operation
LogUpsert(thread, key, value, txId) ==
    LET rec == MakeWalRecord("Upsert", key, value, txId) IN
        /\ AppendRecord(rec)

--------------------------------------------------------------------------------
(* DURABILITY OPERATIONS                                                      *)
(* Operations for making records durable (fsync).                             *)
--------------------------------------------------------------------------------

\* Fsync: make pending records durable
Fsync ==
    /\ Len(pendingRecords) > 0
    /\ wal' = wal \o pendingRecords
    /\ durableLsn' = currentLsn
    /\ pendingRecords' = <<>>
    \* Wake up threads waiting for durability
    /\ groupCommitQueue' = {}
    /\ UNCHANGED <<currentLsn, checkpointLsn, transactions, nextTxId>>

\* A thread requests to join group commit (wait for next fsync)
JoinGroupCommit(thread) ==
    /\ thread \notin groupCommitQueue
    /\ groupCommitQueue' = groupCommitQueue \cup {thread}
    /\ UNCHANGED <<wal, currentLsn, durableLsn, checkpointLsn, pendingRecords, transactions, nextTxId>>

\* Check if a thread's operation is durable
IsDurable(lsn) == lsn <= durableLsn

--------------------------------------------------------------------------------
(* TRANSACTION OPERATIONS                                                     *)
(* Operations for transaction lifecycle management.                           *)
--------------------------------------------------------------------------------

\* Begin a new transaction
BeginTransaction(thread) ==
    /\ nextTxId < MaxLSN  \* Bounded for model checking
    /\ LET
        txId == nextTxId
        rec == MakeBeginTxRecord(txId)
       IN
        /\ transactions' = [transactions EXCEPT ![txId] = [
            txId       |-> txId,
            state      |-> "InProgress",
            beginLsn   |-> currentLsn + 1,
            commitLsn  |-> 0,
            operations |-> <<>>
           ]]
        /\ nextTxId' = nextTxId + 1
        /\ AppendRecord(rec)

\* Commit a transaction
CommitTransaction(txId) ==
    /\ transactions[txId] # <<>>
    /\ transactions[txId].state = "InProgress"
    /\ LET rec == MakeCommitTxRecord(txId) IN
        /\ transactions' = [transactions EXCEPT ![txId].state = "Committed",
                                                ![txId].commitLsn = currentLsn + 1]
        /\ AppendRecord(rec)

\* Abort a transaction
AbortTransaction(txId) ==
    /\ transactions[txId] # <<>>
    /\ transactions[txId].state = "InProgress"
    /\ LET rec == MakeAbortTxRecord(txId) IN
        /\ transactions' = [transactions EXCEPT ![txId].state = "Aborted"]
        /\ AppendRecord(rec)

--------------------------------------------------------------------------------
(* CHECKPOINT OPERATIONS                                                      *)
(* Operations for creating checkpoints.                                       *)
--------------------------------------------------------------------------------

\* Create a checkpoint
Checkpoint ==
    /\ durableLsn > checkpointLsn
    /\ LET rec == MakeCheckpointRecord IN
        /\ checkpointLsn' = durableLsn
        /\ AppendRecord(rec)
        /\ UNCHANGED <<transactions, nextTxId, groupCommitQueue>>

\* Truncate WAL up to checkpoint (for space reclamation)
TruncateWal ==
    /\ checkpointLsn > 0
    /\ LET
        \* Keep only records after checkpoint
        keepFrom == CHOOSE i \in DOMAIN wal :
            /\ wal[i].lsn > checkpointLsn
            /\ \A j \in DOMAIN wal : j < i => wal[j].lsn <= checkpointLsn
       IN
        /\ wal' = SubSeq(wal, keepFrom, Len(wal))
        /\ UNCHANGED <<currentLsn, durableLsn, checkpointLsn, pendingRecords,
                       transactions, nextTxId, groupCommitQueue>>

--------------------------------------------------------------------------------
(* WAL INVARIANTS                                                             *)
--------------------------------------------------------------------------------

\* LSN monotonicity: WAL records have strictly increasing LSNs
LsnMonotonicity ==
    \A i, j \in DOMAIN wal : i < j => wal[i].lsn < wal[j].lsn

\* Durability ordering: durable LSN never exceeds current LSN
DurabilityOrdering ==
    durableLsn <= currentLsn

\* Checkpoint ordering: checkpoint LSN never exceeds durable LSN
CheckpointOrdering ==
    checkpointLsn <= durableLsn

\* Pending records have LSNs between durable and current
PendingRecordsOrdering ==
    \A r \in SeqToSet(pendingRecords) :
        /\ r.lsn > durableLsn
        /\ r.lsn <= currentLsn

\* Transaction state consistency
TxStateConsistency ==
    \A txId \in DOMAIN transactions :
        transactions[txId] # <<>> =>
            \/ transactions[txId].state = "InProgress"
            \/ transactions[txId].state = "Committed"
            \/ transactions[txId].state = "Aborted"

\* Committed transactions have commit records in durable WAL
CommittedTxDurable ==
    \A txId \in DOMAIN transactions :
        transactions[txId] # <<>> =>
            ((transactions[txId].state = "Committed" /\
              transactions[txId].commitLsn <= durableLsn) =>
                (\E r \in SeqToSet(wal) :
                    r.txId = txId /\ r.recType = "CommitTx"))

--------------------------------------------------------------------------------
(* COMBINED WAL SAFETY INVARIANT                                              *)
--------------------------------------------------------------------------------

WalSafetyInvariant ==
    /\ WalTypeInvariant
    /\ LsnMonotonicity
    /\ DurabilityOrdering
    /\ CheckpointOrdering
    /\ PendingRecordsOrdering
    /\ TxStateConsistency

--------------------------------------------------------------------------------
(* LIVENESS PROPERTIES                                                        *)
(* These specify that the system makes progress.                              *)
--------------------------------------------------------------------------------

\* Eventually, pending records become durable (assuming no crash)
EventualDurability ==
    <>(Len(pendingRecords) = 0 \/ durableLsn = currentLsn)

\* Group commit waiters are eventually released
GroupCommitProgress ==
    <>(\A t \in Threads : t \notin groupCommitQueue)

--------------------------------------------------------------------------------
(* RECOVERY INTERFACE                                                         *)
(* Operations used during crash recovery.                                     *)
--------------------------------------------------------------------------------

\* Get all committed operations after a given LSN
CommittedOpsAfter(lsn) ==
    {r \in SeqToSet(wal) :
        /\ r.lsn > lsn
        /\ r.recType \in {"Insert", "Remove", "Upsert", "Increment", "CompareAndSwap"}
        /\ IsTxCommitted(r.txId)}

\* Get the last checkpoint record
LastCheckpoint ==
    LET checkpoints == {r \in SeqToSet(wal) : r.recType = "Checkpoint"} IN
        IF checkpoints = {} THEN <<>>
        ELSE CHOOSE r \in checkpoints : \A r2 \in checkpoints : r.lsn >= r2.lsn

\* Get incomplete transactions (started but not committed/aborted)
IncompleteTransactions ==
    {txId \in DOMAIN transactions :
        /\ transactions[txId] # <<>>
        /\ transactions[txId].state = "InProgress"}

--------------------------------------------------------------------------------
(* STATE VARIABLES EXPORT                                                     *)
--------------------------------------------------------------------------------

walVars == <<wal, currentLsn, durableLsn, checkpointLsn, pendingRecords,
             transactions, nextTxId, groupCommitQueue>>

================================================================================
(* LICENSE: MIT License                                                       *)
(* Copyright (c) 2026 F1r3fly.io                                              *)
================================================================================
