----------------------------- MODULE CrashRecovery -----------------------------
(****************************************************************************)
(* CrashRecovery: ARIES-style crash recovery specification for the          *)
(* Persistent ARTrie. This module models crash behavior and the three-phase *)
(* recovery protocol (Analysis, Redo, Cleanup).                             *)
(*                                                                          *)
(* Key properties verified:                                                 *)
(* 1. Committed operations survive crashes                                  *)
(* 2. Uncommitted operations are rolled back                                *)
(* 3. Recovery restores a consistent state                                  *)
(* 4. Recovery eventually completes (liveness)                              *)
(****************************************************************************)

EXTENDS ARTrieTypes, ARTrieState, WAL, Integers, Sequences, FiniteSets, TLC

--------------------------------------------------------------------------------
(* ADDITIONAL STATE VARIABLES FOR CRASH RECOVERY                              *)
--------------------------------------------------------------------------------

VARIABLES
    \* System has crashed (modeling crash state)
    crashed,

    \* System is in recovery mode
    recovering,

    \* Recovery phase: "None", "Analysis", "Redo", "Cleanup", "Complete"
    recoveryPhase,

    \* State at last checkpoint (snapshot for recovery)
    checkpointState,

    \* Dirty pages that need to be written back
    dirtyPages,

    \* Transactions being analyzed during recovery
    analyzedTxs,

    \* Operations to redo during recovery
    redoQueue,

    \* Recovery statistics
    recoveryStats

--------------------------------------------------------------------------------
(* TYPE DEFINITIONS                                                           *)
--------------------------------------------------------------------------------

RecoveryPhase == {"None", "Analysis", "Redo", "Cleanup", "Complete"}

RecoveryStats == [
    recordsScanned     : Nat,
    validRecords       : Nat,
    corruptedRecords   : Nat,
    committedTxs       : Nat,
    abortedTxs         : Nat,
    incompleteTxs      : Nat,
    insertOps          : Nat,
    removeOps          : Nat,
    durationMs         : Nat
]

--------------------------------------------------------------------------------
(* TYPE INVARIANT                                                             *)
--------------------------------------------------------------------------------

CrashRecoveryTypeInvariant ==
    /\ crashed \in BOOLEAN
    /\ recovering \in BOOLEAN
    /\ recoveryPhase \in RecoveryPhase
    /\ checkpointState \in [Keys -> Values \cup {Null}]
    /\ dirtyPages \in SUBSET NodeIds
    /\ analyzedTxs \in SUBSET Nat
    /\ redoQueue \in Seq(WalRecord)
    /\ recoveryStats \in RecoveryStats

--------------------------------------------------------------------------------
(* INITIAL STATE                                                              *)
--------------------------------------------------------------------------------

CrashRecoveryInit ==
    /\ crashed = FALSE
    /\ recovering = FALSE
    /\ recoveryPhase = "None"
    /\ checkpointState = InitAbstractMap
    /\ dirtyPages = {}
    /\ analyzedTxs = {}
    /\ redoQueue = <<>>
    /\ recoveryStats = [
        recordsScanned     |-> 0,
        validRecords       |-> 0,
        corruptedRecords   |-> 0,
        committedTxs       |-> 0,
        abortedTxs         |-> 0,
        incompleteTxs      |-> 0,
        insertOps          |-> 0,
        removeOps          |-> 0,
        durationMs         |-> 0
       ]

--------------------------------------------------------------------------------
(* CRASH MODELING                                                             *)
(* A crash can occur at any time (if enabled).                                *)
--------------------------------------------------------------------------------

\* System crashes - all in-flight operations are lost
Crash ==
    /\ EnableCrash
    /\ ~crashed
    /\ ~recovering
    /\ crashed' = TRUE
    \* All thread state is reset
    /\ threads' = [t \in Threads |-> InitThreadContext]
    \* All locks are released
    /\ writers' = [nid \in NodeIds |-> 0]
    \* Versions reset to even (stable)
    /\ versions' = [nid \in NodeIds |->
        IF versions[nid] % 2 = 1 THEN versions[nid] + 1 ELSE versions[nid]]
    \* Pending WAL records are lost (not fsynced)
    /\ pendingRecords' = <<>>
    /\ UNCHANGED <<abstractMap, entryCount, root, nodes, nextNodeId,
                   wal, currentLsn, durableLsn, checkpointLsn, transactions,
                   nextTxId, groupCommitQueue, recovering, recoveryPhase,
                   checkpointState, dirtyPages, analyzedTxs, redoQueue,
                   recoveryStats>>

--------------------------------------------------------------------------------
(* RECOVERY PHASE 1: ANALYSIS                                                 *)
(* Scan WAL from last checkpoint to identify committed transactions.          *)
--------------------------------------------------------------------------------

\* Start recovery after a crash
StartRecovery ==
    /\ crashed
    /\ ~recovering
    /\ recovering' = TRUE
    /\ recoveryPhase' = "Analysis"
    /\ crashed' = FALSE
    \* Reset analysis state
    /\ analyzedTxs' = {}
    /\ redoQueue' = <<>>
    /\ recoveryStats' = [recoveryStats EXCEPT
        !.recordsScanned = 0,
        !.validRecords = 0,
        !.committedTxs = 0,
        !.abortedTxs = 0,
        !.incompleteTxs = 0]
    /\ UNCHANGED <<abstractMap, entryCount, root, nodes, nextNodeId, threads,
                   versions, writers, wal, currentLsn, durableLsn, checkpointLsn,
                   pendingRecords, transactions, nextTxId, groupCommitQueue,
                   checkpointState, dirtyPages>>

\* Analyze a single WAL record
AnalyzeRecord(i) ==
    /\ recovering
    /\ recoveryPhase = "Analysis"
    /\ i \in DOMAIN wal
    /\ wal[i].lsn > checkpointLsn
    /\ LET rec == wal[i] IN
        /\ recoveryStats' = [recoveryStats EXCEPT
            !.recordsScanned = @ + 1,
            !.validRecords = @ + 1]
        \* Track transaction state
        /\ IF rec.recType = "BeginTx" THEN
            analyzedTxs' = analyzedTxs \cup {rec.txId}
           ELSE IF rec.recType = "CommitTx" THEN
            /\ analyzedTxs' = analyzedTxs
            /\ recoveryStats' = [recoveryStats EXCEPT !.committedTxs = @ + 1]
           ELSE IF rec.recType = "AbortTx" THEN
            /\ analyzedTxs' = analyzedTxs
            /\ recoveryStats' = [recoveryStats EXCEPT !.abortedTxs = @ + 1]
           ELSE
            /\ analyzedTxs' = analyzedTxs
            /\ UNCHANGED recoveryStats
    /\ UNCHANGED <<abstractMap, entryCount, root, nodes, nextNodeId, threads,
                   versions, writers, wal, currentLsn, durableLsn, checkpointLsn,
                   pendingRecords, transactions, nextTxId, groupCommitQueue,
                   crashed, recovering, recoveryPhase, checkpointState,
                   dirtyPages, redoQueue>>

\* Helper: Filter WAL to get committed operations after checkpoint
\* WAL is already ordered by LSN, so filtering preserves order
FilterCommittedOps(w, cpLsn) ==
    SelectSeq(w, LAMBDA r :
        /\ r.lsn > cpLsn
        /\ r.recType \in {"Insert", "Remove", "Upsert", "Increment"}
        /\ IsTxCommitted(r.txId))

\* Complete analysis phase
CompleteAnalysis ==
    /\ recovering
    /\ recoveryPhase = "Analysis"
    \* All records after checkpoint have been analyzed
    /\ \A i \in DOMAIN wal : wal[i].lsn > checkpointLsn =>
        recoveryStats.recordsScanned >= (i - checkpointLsn)
    \* Build redo queue: committed operations only (already ordered by LSN in WAL)
    /\ redoQueue' = FilterCommittedOps(wal, checkpointLsn)
    /\ recoveryPhase' = "Redo"
    /\ UNCHANGED <<abstractMap, entryCount, root, nodes, nextNodeId, threads,
                   versions, writers, wal, currentLsn, durableLsn, checkpointLsn,
                   pendingRecords, transactions, nextTxId, groupCommitQueue,
                   crashed, recovering, checkpointState, dirtyPages,
                   analyzedTxs, recoveryStats>>

--------------------------------------------------------------------------------
(* RECOVERY PHASE 2: REDO                                                     *)
(* Replay committed operations to rebuild state.                              *)
--------------------------------------------------------------------------------

\* Redo a single operation
RedoOperation ==
    /\ recovering
    /\ recoveryPhase = "Redo"
    /\ Len(redoQueue) > 0
    /\ LET rec == Head(redoQueue) IN
        /\ IF rec.recType = "Insert" THEN
            /\ abstractMap' = [abstractMap EXCEPT ![rec.key] = rec.value]
            /\ entryCount' = IF abstractMap[rec.key] = Null
                             THEN entryCount + 1
                             ELSE entryCount
            /\ recoveryStats' = [recoveryStats EXCEPT !.insertOps = @ + 1]
           ELSE IF rec.recType = "Remove" THEN
            /\ abstractMap' = [abstractMap EXCEPT ![rec.key] = Null]
            /\ entryCount' = IF abstractMap[rec.key] # Null
                             THEN entryCount - 1
                             ELSE entryCount
            /\ recoveryStats' = [recoveryStats EXCEPT !.removeOps = @ + 1]
           ELSE IF rec.recType = "Upsert" THEN
            /\ abstractMap' = [abstractMap EXCEPT ![rec.key] = rec.value]
            /\ entryCount' = IF abstractMap[rec.key] = Null
                             THEN entryCount + 1
                             ELSE entryCount
            /\ recoveryStats' = [recoveryStats EXCEPT !.insertOps = @ + 1]
           ELSE
            /\ UNCHANGED <<abstractMap, entryCount, recoveryStats>>
        /\ redoQueue' = Tail(redoQueue)
    /\ UNCHANGED <<root, nodes, nextNodeId, threads, versions, writers,
                   wal, currentLsn, durableLsn, checkpointLsn, pendingRecords,
                   transactions, nextTxId, groupCommitQueue, crashed,
                   recovering, recoveryPhase, checkpointState, dirtyPages,
                   analyzedTxs>>

\* Complete redo phase
CompleteRedo ==
    /\ recovering
    /\ recoveryPhase = "Redo"
    /\ Len(redoQueue) = 0
    /\ recoveryPhase' = "Cleanup"
    /\ UNCHANGED <<abstractMap, entryCount, root, nodes, nextNodeId, threads,
                   versions, writers, wal, currentLsn, durableLsn, checkpointLsn,
                   pendingRecords, transactions, nextTxId, groupCommitQueue,
                   crashed, recovering, checkpointState, dirtyPages,
                   analyzedTxs, redoQueue, recoveryStats>>

--------------------------------------------------------------------------------
(* RECOVERY PHASE 3: CLEANUP                                                  *)
(* Truncate WAL and finalize recovery.                                        *)
--------------------------------------------------------------------------------

\* Cleanup after redo
Cleanup ==
    /\ recovering
    /\ recoveryPhase = "Cleanup"
    \* Mark incomplete transactions as aborted
    /\ transactions' = [txId \in DOMAIN transactions |->
        IF txId \in analyzedTxs /\ transactions[txId].state = "InProgress"
        THEN [transactions[txId] EXCEPT !.state = "Aborted"]
        ELSE transactions[txId]]
    \* Update incomplete transaction count
    /\ recoveryStats' = [recoveryStats EXCEPT
        !.incompleteTxs = Cardinality(analyzedTxs \cap IncompleteTransactions)]
    /\ recoveryPhase' = "Complete"
    /\ UNCHANGED <<abstractMap, entryCount, root, nodes, nextNodeId, threads,
                   versions, writers, wal, currentLsn, durableLsn, checkpointLsn,
                   pendingRecords, nextTxId, groupCommitQueue, crashed,
                   recovering, checkpointState, dirtyPages, analyzedTxs,
                   redoQueue>>

\* Complete recovery and return to normal operation
CompleteRecovery ==
    /\ recovering
    /\ recoveryPhase = "Complete"
    /\ recovering' = FALSE
    /\ recoveryPhase' = "None"
    \* Update checkpoint state to current state
    /\ checkpointState' = abstractMap
    /\ dirtyPages' = {}
    /\ UNCHANGED <<abstractMap, entryCount, root, nodes, nextNodeId, threads,
                   versions, writers, wal, currentLsn, durableLsn, checkpointLsn,
                   pendingRecords, transactions, nextTxId, groupCommitQueue,
                   crashed, analyzedTxs, redoQueue, recoveryStats>>

--------------------------------------------------------------------------------
(* CHECKPOINT STATE MANAGEMENT                                                *)
(* Checkpoints capture a consistent snapshot for recovery.                    *)
--------------------------------------------------------------------------------

\* Take a checkpoint (snapshot current state)
TakeCheckpoint ==
    /\ ~crashed
    /\ ~recovering
    /\ checkpointState' = abstractMap
    /\ dirtyPages' = {}
    \* The actual checkpoint LSN update is handled in WAL module
    /\ UNCHANGED <<abstractMap, entryCount, root, nodes, nextNodeId, threads,
                   versions, writers, crashed, recovering, recoveryPhase,
                   analyzedTxs, redoQueue, recoveryStats>>

\* Mark a page as dirty
\* Note: abstractMap and entryCount are NOT in UNCHANGED because this action
\* is composed with operations that modify them (Insert/Remove in PART.tla)
MarkDirty(nid) ==
    /\ ValidNode(nid)
    /\ dirtyPages' = dirtyPages \cup {nid}
    /\ UNCHANGED <<root, nodes, nextNodeId, threads,
                   versions, writers, crashed, recovering, recoveryPhase,
                   checkpointState, analyzedTxs, redoQueue, recoveryStats>>

\* Write back a dirty page
WriteBackPage(nid) ==
    /\ nid \in dirtyPages
    /\ dirtyPages' = dirtyPages \ {nid}
    /\ UNCHANGED <<abstractMap, entryCount, root, nodes, nextNodeId, threads,
                   versions, writers, crashed, recovering, recoveryPhase,
                   checkpointState, analyzedTxs, redoQueue, recoveryStats>>

--------------------------------------------------------------------------------
(* CRASH RECOVERY INVARIANTS                                                  *)
--------------------------------------------------------------------------------

\* Committed transactions survive crashes
CommittedSurvivesCrash ==
    \* After recovery, all committed operations before the crash are present
    recovering' = FALSE /\ recoveryPhase' = "None" =>
        \A r \in SeqToSet(wal) :
            /\ r.lsn <= durableLsn
            /\ r.recType = "Insert"
            /\ IsTxCommitted(r.txId)
            => abstractMap[r.key] # Null

\* Recovery restores consistent state
RecoveryConsistency ==
    recoveryPhase = "Complete" =>
        \* Abstract map matches committed operations
        \A k \in Keys :
            \/ (\E r \in SeqToSet(wal) :
                /\ r.key = k
                /\ r.lsn > checkpointLsn
                /\ r.lsn <= durableLsn
                /\ IsTxCommitted(r.txId)
                /\ r.recType = "Insert"
                /\ abstractMap[k] = r.value)
            \/ (\A r \in SeqToSet(wal) :
                r.key = k /\ r.lsn > checkpointLsn /\ IsTxCommitted(r.txId) =>
                    r.recType = "Remove" \/ abstractMap[k] = checkpointState[k])

\* Not recovering and not crashed is normal operation
NormalOperation ==
    ~recovering /\ ~crashed

\* System is either normal, crashed, or recovering (not multiple)
ExclusiveStates ==
    /\ ~(crashed /\ recovering)

\* Recovery phases are sequential
RecoveryPhasesSequential ==
    recovering =>
        \/ recoveryPhase = "Analysis"
        \/ recoveryPhase = "Redo"
        \/ recoveryPhase = "Cleanup"
        \/ recoveryPhase = "Complete"

--------------------------------------------------------------------------------
(* COMBINED CRASH RECOVERY SAFETY INVARIANT                                   *)
--------------------------------------------------------------------------------

CrashRecoverySafetyInvariant ==
    /\ CrashRecoveryTypeInvariant
    /\ ExclusiveStates
    /\ RecoveryPhasesSequential

--------------------------------------------------------------------------------
(* LIVENESS PROPERTIES                                                        *)
--------------------------------------------------------------------------------

\* Recovery eventually completes
RecoveryEventuallyCompletes ==
    crashed => <>(~recovering)

\* After crash, system eventually returns to normal operation
EventualNormalOperation ==
    crashed => <>(NormalOperation)

--------------------------------------------------------------------------------
(* ACTIONS                                                                    *)
(* Combined actions for crash recovery.                                       *)
--------------------------------------------------------------------------------

\* The crash action
CrashAction ==
    /\ Crash
    /\ UNCHANGED <<wal, currentLsn, durableLsn, checkpointLsn,
                   transactions, nextTxId, recovering, recoveryPhase,
                   checkpointState, dirtyPages, analyzedTxs, redoQueue,
                   recoveryStats>>

\* The full recovery sequence
RecoverAction ==
    \/ StartRecovery
    \/ \E i \in DOMAIN wal : AnalyzeRecord(i)
    \/ CompleteAnalysis
    \/ RedoOperation
    \/ CompleteRedo
    \/ Cleanup
    \/ CompleteRecovery

--------------------------------------------------------------------------------
(* STATE VARIABLES EXPORT                                                     *)
--------------------------------------------------------------------------------

crashRecoveryVars == <<crashed, recovering, recoveryPhase, checkpointState,
                       dirtyPages, analyzedTxs, redoQueue, recoveryStats>>

================================================================================
(* LICENSE: MIT License                                                       *)
(* Copyright (c) 2026 F1r3fly.io                                              *)
================================================================================
