--------------------------------- MODULE PART ----------------------------------
(****************************************************************************)
(* PART: Persistent Adaptive Radix Trie - Main Composed Specification       *)
(*                                                                          *)
(* This module composes all sub-specifications into the complete ARTrie     *)
(* specification, defining the full state machine and verification          *)
(* properties.                                                              *)
(*                                                                          *)
(* Components:                                                              *)
(* - ARTrieTypes: Type definitions and constants                            *)
(* - ARTrieState: Abstract trie state and structural invariants             *)
(* - WAL: Write-ahead log for durability                                    *)
(* - Concurrency: Optimistic lock coupling                                  *)
(* - CrashRecovery: ARIES-style crash recovery                              *)
(* - NodeTransitions: Node type growth/shrink                               *)
(* - EpochCheckpoint: Epoch-based checkpointing                             *)
(*                                                                          *)
(* Properties Verified:                                                     *)
(* SAFETY:                                                                  *)
(*   - Completeness: All inserted items are retrievable                     *)
(*   - Consistency: Removed items are not retrievable                       *)
(*   - Exclusive Write: No concurrent writers to same node                  *)
(*   - Version Consistency: Odd version implies exclusively locked          *)
(*   - Node Capacity: Node type respects child count limits                 *)
(*   - Transition Correctness: Node transitions preserve all children       *)
(*   - Crash Recovery: Committed operations survive crashes                 *)
(*   - Linearizability: Operations appear atomic                            *)
(*                                                                          *)
(* LIVENESS:                                                                *)
(*   - Writers eventually release locks                                     *)
(*   - Recovery eventually completes                                        *)
(*   - Checkpoints eventually complete                                      *)
(****************************************************************************)

EXTENDS ARTrieTypes, ARTrieState, WAL, Concurrency, CrashRecovery,
        NodeTransitions, EpochCheckpoint, Integers, Sequences, FiniteSets, TLC

--------------------------------------------------------------------------------
(* SENTINEL CONSTANTS                                                         *)
(* Used for indicating absence in CHOOSE expressions.                         *)
--------------------------------------------------------------------------------

\* Null record for Linearizable property - represents "no matching WAL record"
NullRecord == [
    lsn           |-> 0,
    recType       |-> "Insert",
    txId          |-> 0,
    key           |-> Null,
    value         |-> Null,
    expected      |-> Null,
    delta         |-> 0,
    checkpointLsn |-> 0
]

--------------------------------------------------------------------------------
(* COMBINED STATE VARIABLES                                                   *)
(* All state variables from all modules.                                      *)
--------------------------------------------------------------------------------

\* From ARTrieState
\* abstractMap, entryCount, root, nodes, nextNodeId, threads, versions, writers

\* From WAL
\* wal, currentLsn, durableLsn, checkpointLsn, pendingRecords,
\* transactions, nextTxId, groupCommitQueue

\* From Concurrency
\* readGuards, writeGuards, globalEpoch, activeReaders, threadEpoch,
\* retryStats, lockDepth

\* From CrashRecovery
\* crashed, recovering, recoveryPhase, checkpointState, dirtyPages,
\* analyzedTxs, redoQueue, recoveryStats

\* From EpochCheckpoint
\* currentEpochId, epochs, currentEpochOps, currentEpochWalSize,
\* checkpointInProgress, checkpointPages, lastCheckpointEpoch

--------------------------------------------------------------------------------
(* FULL STATE VARIABLE TUPLE                                                  *)
--------------------------------------------------------------------------------

allVars == <<
    \* ARTrieState
    abstractMap, entryCount, root, nodes, nextNodeId, threads, versions, writers,
    \* WAL
    wal, currentLsn, durableLsn, checkpointLsn, pendingRecords,
    transactions, nextTxId, groupCommitQueue,
    \* Concurrency
    readGuards, writeGuards, globalEpoch, activeReaders, threadEpoch,
    retryStats, lockDepth,
    \* CrashRecovery
    crashed, recovering, recoveryPhase, checkpointState, dirtyPages,
    analyzedTxs, redoQueue, recoveryStats,
    \* EpochCheckpoint
    currentEpochId, epochs, currentEpochOps, currentEpochWalSize,
    checkpointInProgress, checkpointPages, lastCheckpointEpoch
>>

--------------------------------------------------------------------------------
(* INITIAL STATE                                                              *)
(* Compose all module initializations.                                        *)
--------------------------------------------------------------------------------

PARTInit ==
    /\ Init           \* ARTrieState
    /\ WalInit        \* WAL
    /\ ConcurrencyInit \* Concurrency
    /\ CrashRecoveryInit \* CrashRecovery
    /\ EpochInit      \* EpochCheckpoint

--------------------------------------------------------------------------------
(* HIGH-LEVEL OPERATIONS                                                      *)
(* The main operations exposed by the ARTrie.                                 *)
--------------------------------------------------------------------------------

\* Lookup operation: read a value for a key
\* NOTE: Composed inline to avoid conflicting UNCHANGED clauses
Lookup(thread, key) ==
    /\ ~crashed
    /\ ~recovering
    /\ ThreadIdle(thread)
    /\ EpochInState(currentEpochId, "Active")
    /\ LET
        rootNid == root.memPtr
        e == globalEpoch
        v == versions[rootNid]
       IN
        \* Preconditions
        /\ ValidNode(rootNid)
        /\ VersionStable(v)
        \* State updates from EnterReadEpoch
        /\ threadEpoch' = [threadEpoch EXCEPT ![thread] = e]
        /\ activeReaders' = [activeReaders EXCEPT ![e] = @ + 1]
        \* State updates from BeginOptimisticRead
        /\ readGuards' = [readGuards EXCEPT ![thread] = @ \cup {<<rootNid, v>>}]
        \* Thread state update
        /\ threads' = [threads EXCEPT
            ![thread].state = "Reading",
            ![thread].operationType = "Lookup",
            ![thread].targetKey = key,
            ![thread].currentNode = rootNid]
    /\ UNCHANGED <<abstractMap, entryCount, root, nodes, nextNodeId,
                   versions, writers, writeGuards, globalEpoch, retryStats, lockDepth,
                   walVars, crashRecoveryVars, epochVars>>

\* Complete a lookup operation
\* NOTE: Composed inline to avoid conflicting UNCHANGED clauses
CompleteLookup(thread) ==
    /\ ThreadReading(thread)
    /\ threads[thread].operationType = "Lookup"
    /\ LET
        key == threads[thread].targetKey
        nid == threads[thread].currentNode
        e == threadEpoch[thread]
       IN
        \* Validate the read (check version hasn't changed)
        /\ \E g \in readGuards[thread] : g[1] = nid /\ g[2] = versions[nid]
        \* State updates from EndOptimisticRead
        /\ readGuards' = [readGuards EXCEPT ![thread] = {g \in @ : g[1] # nid}]
        \* State updates from ExitReadEpoch
        /\ activeReaders' = [activeReaders EXCEPT ![e] = @ - 1]
        /\ threadEpoch' = [threadEpoch EXCEPT ![thread] = 0]
        \* Thread state update
        /\ threads' = [threads EXCEPT
            ![thread].state = "Idle",
            ![thread].operationType = "None",
            ![thread].targetKey = Null,
            ![thread].currentNode = Null]
    /\ UNCHANGED <<abstractMap, entryCount, root, nodes, nextNodeId,
                   versions, writers, writeGuards, globalEpoch, retryStats, lockDepth,
                   walVars, crashRecoveryVars, epochVars>>

\* Insert operation: associate a key with a value
\* NOTE: Composed inline to avoid conflicting UNCHANGED clauses
Insert(thread, key, value) ==
    /\ ~crashed
    /\ ~recovering
    /\ ThreadIdle(thread)
    /\ EpochInState(currentEpochId, "Active")
    /\ key \in Keys
    /\ value \in Values
    /\ LET rootNid == root.memPtr IN
        \* Preconditions from BeginWrite
        /\ ValidNode(rootNid)
        /\ writers[rootNid] = 0
        /\ VersionStable(versions[rootNid])
        \* State updates from BeginWrite
        /\ versions' = [versions EXCEPT ![rootNid] = @ + 1]
        /\ writers' = [writers EXCEPT ![rootNid] = thread]
        /\ writeGuards' = [writeGuards EXCEPT ![thread] = Append(@, rootNid)]
        /\ lockDepth' = [lockDepth EXCEPT ![thread] = @ + 1]
        \* Thread state update
        /\ threads' = [threads EXCEPT
            ![thread].state = "Writing",
            ![thread].operationType = "Insert",
            ![thread].targetKey = key,
            ![thread].targetValue = value,
            ![thread].currentNode = rootNid]
    /\ UNCHANGED <<abstractMap, entryCount, root, nodes, nextNodeId,
                   readGuards, globalEpoch, activeReaders, threadEpoch, retryStats,
                   walVars, crashRecoveryVars, epochVars>>

\* Complete an insert operation
\* NOTE: Composed inline to avoid conflicting UNCHANGED clauses
CompleteInsert(thread) ==
    /\ ThreadWriting(thread)
    /\ threads[thread].operationType = "Insert"
    /\ EpochInState(currentEpochId, "Active")
    /\ LET
        key == threads[thread].targetKey
        value == threads[thread].targetValue
        nid == threads[thread].currentNode
        guards == writeGuards[thread]
        rec == [
            lsn           |-> currentLsn + 1,
            recType       |-> "Insert",
            txId          |-> 0,
            key           |-> key,
            value         |-> value,
            expected      |-> <<>>,
            delta         |-> 0,
            checkpointLsn |-> 0
        ]
       IN
        \* Preconditions
        /\ currentLsn < MaxLSN
        /\ HoldsWriteLock(thread, nid)
        /\ Len(guards) > 0
        /\ guards[Len(guards)] = nid
        \* Update abstract map
        /\ abstractMap' = AbstractInsert(key, value)
        /\ entryCount' = IF abstractMap[key] = Null THEN entryCount + 1 ELSE entryCount
        \* State updates from LogInsert (AppendRecord)
        /\ currentLsn' = currentLsn + 1
        /\ pendingRecords' = Append(pendingRecords, rec)
        \* State updates from MarkDirty
        /\ dirtyPages' = dirtyPages \cup {nid}
        \* State updates from RecordOperation
        /\ currentEpochOps' = currentEpochOps + 1
        /\ currentEpochWalSize' = currentEpochWalSize + 1
        /\ epochs' = [epochs EXCEPT
            ![currentEpochId].operationCount = @ + 1,
            ![currentEpochId].lastLsn = currentLsn + 1]  \* Use new LSN value
        \* State updates from EndWrite
        /\ versions' = [versions EXCEPT ![nid] = @ + 1]
        /\ writers' = [writers EXCEPT ![nid] = 0]
        /\ writeGuards' = [writeGuards EXCEPT ![thread] = SubSeq(@, 1, Len(@) - 1)]
        /\ lockDepth' = [lockDepth EXCEPT ![thread] = @ - 1]
        /\ retryStats' = [retryStats EXCEPT ![thread].successful = @ + 1]
        \* Thread state update
        /\ threads' = [threads EXCEPT
            ![thread].state = "Idle",
            ![thread].operationType = "None",
            ![thread].targetKey = Null,
            ![thread].targetValue = Null,
            ![thread].currentNode = Null]
    /\ UNCHANGED <<root, nodes, nextNodeId, readGuards, globalEpoch, activeReaders,
                   threadEpoch, wal, durableLsn, checkpointLsn, transactions, nextTxId,
                   groupCommitQueue, crashed, recovering, recoveryPhase, checkpointState,
                   analyzedTxs, redoQueue, recoveryStats, currentEpochId,
                   checkpointInProgress, checkpointPages, lastCheckpointEpoch>>

\* Remove operation: remove a key
\* NOTE: Composed inline to avoid conflicting UNCHANGED clauses
Remove(thread, key) ==
    /\ ~crashed
    /\ ~recovering
    /\ ThreadIdle(thread)
    /\ EpochInState(currentEpochId, "Active")
    /\ key \in Keys
    /\ abstractMap[key] # Null  \* Key exists
    /\ LET rootNid == root.memPtr IN
        \* Preconditions from BeginWrite
        /\ ValidNode(rootNid)
        /\ writers[rootNid] = 0
        /\ VersionStable(versions[rootNid])
        \* State updates from BeginWrite
        /\ versions' = [versions EXCEPT ![rootNid] = @ + 1]
        /\ writers' = [writers EXCEPT ![rootNid] = thread]
        /\ writeGuards' = [writeGuards EXCEPT ![thread] = Append(@, rootNid)]
        /\ lockDepth' = [lockDepth EXCEPT ![thread] = @ + 1]
        \* Thread state update
        /\ threads' = [threads EXCEPT
            ![thread].state = "Writing",
            ![thread].operationType = "Remove",
            ![thread].targetKey = key,
            ![thread].currentNode = rootNid]
    /\ UNCHANGED <<abstractMap, entryCount, root, nodes, nextNodeId,
                   readGuards, globalEpoch, activeReaders, threadEpoch, retryStats,
                   walVars, crashRecoveryVars, epochVars>>

\* Complete a remove operation
\* NOTE: Composed inline to avoid conflicting UNCHANGED clauses
CompleteRemove(thread) ==
    /\ ThreadWriting(thread)
    /\ threads[thread].operationType = "Remove"
    /\ EpochInState(currentEpochId, "Active")
    /\ LET
        key == threads[thread].targetKey
        nid == threads[thread].currentNode
        guards == writeGuards[thread]
        rec == [
            lsn           |-> currentLsn + 1,
            recType       |-> "Remove",
            txId          |-> 0,
            key           |-> key,
            value         |-> Null,
            expected      |-> <<>>,
            delta         |-> 0,
            checkpointLsn |-> 0
        ]
       IN
        \* Preconditions
        /\ currentLsn < MaxLSN
        /\ HoldsWriteLock(thread, nid)
        /\ Len(guards) > 0
        /\ guards[Len(guards)] = nid
        \* Update abstract map
        /\ abstractMap' = AbstractRemove(key)
        /\ entryCount' = entryCount - 1
        \* State updates from LogRemove (AppendRecord)
        /\ currentLsn' = currentLsn + 1
        /\ pendingRecords' = Append(pendingRecords, rec)
        \* State updates from MarkDirty
        /\ dirtyPages' = dirtyPages \cup {nid}
        \* State updates from RecordOperation
        /\ currentEpochOps' = currentEpochOps + 1
        /\ currentEpochWalSize' = currentEpochWalSize + 1
        /\ epochs' = [epochs EXCEPT
            ![currentEpochId].operationCount = @ + 1,
            ![currentEpochId].lastLsn = currentLsn + 1]  \* Use new LSN value
        \* State updates from EndWrite
        /\ versions' = [versions EXCEPT ![nid] = @ + 1]
        /\ writers' = [writers EXCEPT ![nid] = 0]
        /\ writeGuards' = [writeGuards EXCEPT ![thread] = SubSeq(@, 1, Len(@) - 1)]
        /\ lockDepth' = [lockDepth EXCEPT ![thread] = @ - 1]
        /\ retryStats' = [retryStats EXCEPT ![thread].successful = @ + 1]
        \* Thread state update
        /\ threads' = [threads EXCEPT
            ![thread].state = "Idle",
            ![thread].operationType = "None",
            ![thread].targetKey = Null,
            ![thread].currentNode = Null]
    /\ UNCHANGED <<root, nodes, nextNodeId, readGuards, globalEpoch, activeReaders,
                   threadEpoch, wal, durableLsn, checkpointLsn, transactions, nextTxId,
                   groupCommitQueue, crashed, recovering, recoveryPhase, checkpointState,
                   analyzedTxs, redoQueue, recoveryStats, currentEpochId,
                   checkpointInProgress, checkpointPages, lastCheckpointEpoch>>

--------------------------------------------------------------------------------
(* INTERNAL OPERATIONS                                                        *)
(* Operations that happen as part of other operations.                        *)
--------------------------------------------------------------------------------

\* Grow a node during insertion
GrowNodeDuringInsert(thread, nid) ==
    /\ ThreadWriting(thread)
    /\ HoldsWriteLock(thread, nid)
    /\ GrowAction(thread, nid)
    /\ UNCHANGED <<abstractMap, entryCount, root, threads,
                   walVars, crashRecoveryVars, epochVars>>

\* Shrink a node during removal
ShrinkNodeDuringRemove(thread, nid) ==
    /\ ThreadWriting(thread)
    /\ HoldsWriteLock(thread, nid)
    /\ ShrinkAction(thread, nid)
    /\ UNCHANGED <<abstractMap, entryCount, root, threads,
                   walVars, crashRecoveryVars, epochVars>>

\* Fsync WAL for durability
FsyncWal ==
    /\ ~crashed
    /\ ~recovering
    /\ Fsync
    /\ UNCHANGED <<abstractMap, entryCount, root, nodes, nextNodeId,
                   threads, versions, writers, concurrencyVars,
                   crashRecoveryVars, epochVars>>

--------------------------------------------------------------------------------
(* SYSTEM ACTIONS                                                             *)
(* Background system operations.                                              *)
--------------------------------------------------------------------------------

\* System crash (if enabled)
\* Crash in CrashRecovery.tla handles threads, writers, versions, pendingRecords.
\* We reset concurrency variables here (guards, epoch tracking).
SystemCrash ==
    /\ Crash
    /\ readGuards' = [t \in Threads |-> {}]
    /\ writeGuards' = [t \in Threads |-> <<>>]
    /\ lockDepth' = [t \in Threads |-> 0]
    /\ threadEpoch' = [t \in Threads |-> 0]
    /\ activeReaders' = [e \in 0..MaxEpoch |-> 0]
    /\ UNCHANGED <<wal, currentLsn, durableLsn, checkpointLsn, transactions,
                   nextTxId, groupCommitQueue, epochVars, globalEpoch, retryStats>>

\* System recovery
SystemRecover ==
    /\ RecoverAction
    /\ UNCHANGED <<abstractMap, entryCount, root, nodes, nextNodeId,
                   threads, versions, writers, concurrencyVars, epochVars>>

\* Checkpoint cycle
SystemCheckpoint ==
    /\ ~crashed
    /\ ~recovering
    /\ CheckpointCycle
    /\ UNCHANGED <<abstractMap, entryCount, root, nodes, nextNodeId,
                   threads, versions, writers, walVars, concurrencyVars,
                   crashRecoveryVars>>

\* Advance global epoch for reclamation
\* NOTE: AdvanceEpoch changes currentEpochId, epochs, currentEpochOps, currentEpochWalSize
AdvanceGlobalEpochAction ==
    /\ ~crashed
    /\ ~recovering
    /\ AdvanceEpoch
    /\ UNCHANGED <<abstractMap, entryCount, root, nodes, nextNodeId,
                   threads, versions, writers, walVars, crashRecoveryVars,
                   checkpointInProgress, checkpointPages, lastCheckpointEpoch,
                   concurrencyVars>>

--------------------------------------------------------------------------------
(* NEXT STATE RELATION                                                        *)
(* The complete next-state relation.                                          *)
--------------------------------------------------------------------------------

\* Thread actions
ThreadAction(thread) ==
    \/ (\E k \in Keys : Lookup(thread, k))
    \/ CompleteLookup(thread)
    \/ (\E k \in Keys : \E v \in Values : Insert(thread, k, v))
    \/ CompleteInsert(thread)
    \/ (\E k \in Keys : abstractMap[k] # Null /\ Remove(thread, k))
    \/ CompleteRemove(thread)
    \/ (\E nid \in NodeIds : GrowNodeDuringInsert(thread, nid))
    \/ (\E nid \in NodeIds : ShrinkNodeDuringRemove(thread, nid))

\* System actions
SystemAction ==
    \/ FsyncWal
    \/ SystemCrash
    \/ SystemRecover
    \/ SystemCheckpoint
    \/ AdvanceGlobalEpochAction

\* Full next-state relation
Next ==
    \/ (\E t \in Threads : ThreadAction(t))
    \/ SystemAction

--------------------------------------------------------------------------------
(* FAIRNESS CONDITIONS                                                        *)
(* Weak fairness ensures the system makes progress.                           *)
--------------------------------------------------------------------------------

Fairness ==
    /\ WF_allVars(FsyncWal)
    /\ WF_allVars(SystemRecover)
    /\ WF_allVars(SystemCheckpoint)
    /\ \A t \in Threads :
        /\ WF_allVars(CompleteLookup(t))
        /\ WF_allVars(CompleteInsert(t))
        /\ WF_allVars(CompleteRemove(t))

--------------------------------------------------------------------------------
(* SPECIFICATION                                                              *)
(* The full specification with fairness.                                      *)
--------------------------------------------------------------------------------

Spec == PARTInit /\ [][Next]_allVars /\ Fairness

--------------------------------------------------------------------------------
(* COMBINED SAFETY INVARIANTS                                                 *)
(* All safety properties that must hold.                                      *)
--------------------------------------------------------------------------------

\* From ARTrieState
\* SafetyInvariant (includes TypeInvariant, AllNodesWellFormed,
\*                  CompletenessInvariant, ConsistencyInvariant,
\*                  VersionLockConsistency, ExclusiveWriteInvariant)

\* From WAL
\* WalSafetyInvariant (includes WalTypeInvariant, LsnMonotonicity,
\*                     DurabilityOrdering, CheckpointOrdering,
\*                     PendingRecordsOrdering, TxStateConsistency)

\* From Concurrency
\* ConcurrencySafetyInvariant (includes ConcurrencyTypeInvariant,
\*                             SingleWriterInvariant, VersionWriterConsistency,
\*                             LockDepthConsistency, ReadGuardStability)

\* From CrashRecovery
\* CrashRecoverySafetyInvariant (includes CrashRecoveryTypeInvariant,
\*                               ExclusiveStates, RecoveryPhasesSequential)

\* From NodeTransitions
\* NodeTransitionSafetyInvariant (AllNodesAppropriateType)

\* From EpochCheckpoint
\* EpochSafetyInvariant (includes EpochTypeInvariant, EpochTransitionOrder,
\*                       SingleActiveEpoch, SingleSealingEpoch, etc.)

\* Combined safety invariant
CombinedSafetyInvariant ==
    /\ SafetyInvariant
    /\ WalSafetyInvariant
    /\ ConcurrencySafetyInvariant
    /\ CrashRecoverySafetyInvariant
    /\ NodeTransitionSafetyInvariant
    /\ EpochSafetyInvariant

--------------------------------------------------------------------------------
(* LINEARIZABILITY                                                            *)
(* Operations appear atomic with respect to the abstract map.                 *)
--------------------------------------------------------------------------------

\* Define linearization points
LinearizationPointInsert(thread) ==
    /\ threads[thread].state = "Writing"
    /\ threads[thread].operationType = "Insert"
    \* Linearization point is when abstract map is updated

LinearizationPointRemove(thread) ==
    /\ threads[thread].state = "Writing"
    /\ threads[thread].operationType = "Remove"
    \* Linearization point is when abstract map is updated

LinearizationPointLookup(thread) ==
    /\ threads[thread].state = "Reading"
    /\ threads[thread].operationType = "Lookup"
    \* Linearization point is when read completes successfully

\* Linearizability: abstract map reflects committed operations
Linearizable ==
    \* The abstract map state at any point is consistent with
    \* a sequential history of committed operations
    \A k \in Keys :
        abstractMap[k] = (
            LET
                \* Last committed write to this key
                lastWrite == CHOOSE r \in SeqToSet(wal) \cup {NullRecord} :
                    /\ (r = NullRecord \/ (r.key = k /\ r.recType \in {"Insert", "Remove"}))
                    /\ (r = NullRecord \/ r.lsn <= durableLsn)
                    /\ (\A r2 \in SeqToSet(wal) :
                        (r2.key = k /\ r2.recType \in {"Insert", "Remove"} /\
                         r2.lsn <= durableLsn) => r2.lsn <= r.lsn)
            IN
                IF lastWrite = NullRecord THEN Null
                ELSE IF lastWrite.recType = "Remove" THEN Null
                ELSE lastWrite.value
        )

--------------------------------------------------------------------------------
(* COMBINED LIVENESS PROPERTIES                                               *)
(* All liveness properties that must hold (with fairness).                    *)
--------------------------------------------------------------------------------

\* From WAL: EventualDurability, GroupCommitProgress
\* From Concurrency: WritersEventuallyRelease, ReadsEventuallySucceed
\* From CrashRecovery: RecoveryEventuallyCompletes, EventualNormalOperation
\* From EpochCheckpoint: CheckpointEventuallyCompletes, EpochsEventuallyAdvance

\* Operations eventually complete
OperationsEventuallyComplete ==
    \A t \in Threads :
        threads[t].state # "Idle" => <>(threads[t].state = "Idle")

\* System eventually returns to normal after crash
SystemEventuallyNormal ==
    crashed => <>(~crashed /\ ~recovering)

\* Combined liveness
CombinedLiveness ==
    /\ EventualDurability
    /\ WritersEventuallyRelease
    /\ RecoveryEventuallyCompletes
    /\ CheckpointEventuallyCompletes
    /\ OperationsEventuallyComplete
    /\ SystemEventuallyNormal

--------------------------------------------------------------------------------
(* PROPERTIES TO VERIFY                                                       *)
(* Summary of all properties for TLC model checking.                          *)
--------------------------------------------------------------------------------

\* Safety properties (invariants)
PROPERTY_Safety == []CombinedSafetyInvariant

\* Liveness properties (requires fairness)
PROPERTY_Liveness == CombinedLiveness

\* Crash recovery safety: committed ops survive
\* After recovery completes, committed operations are present
PROPERTY_CrashRecovery ==
    [](recoveryPhase = "Complete" => RecoveryConsistency)

\* Node transition correctness
PROPERTY_NodeTransitions ==
    []NodeTransitionSafetyInvariant

================================================================================
(* VERIFICATION COMMANDS                                                      *)
(*                                                                            *)
(* Run TLC with:                                                              *)
(*   tlc -workers 8 PART.tla -config PART.cfg                                 *)
(*                                                                            *)
(* Suggested PART.cfg:                                                        *)
(*   CONSTANTS                                                                *)
(*     NumThreads = 3                                                         *)
(*     MaxKeys = 4                                                            *)
(*     MaxKeyLength = 3                                                       *)
(*     MaxLSN = 15                                                            *)
(*     MaxEpoch = 3                                                           *)
(*     EnableCrash = TRUE                                                     *)
(*     MaxOpsPerEpoch = 5                                                     *)
(*     MaxWalSizePerEpoch = 10                                                *)
(*     RetentionEpochs = 2                                                    *)
(*     BackgroundCheckpoint = TRUE                                            *)
(*     IncrementalCheckpoint = TRUE                                           *)
(*     NodeIds = {1, 2, 3, 4, 5, 6, 7, 8}                                     *)
(*     Keys = {"a", "ab", "abc", "b"}                                         *)
(*     Values = {1, 2, 3}                                                     *)
(*                                                                            *)
(*   SPECIFICATION Spec                                                       *)
(*   INVARIANT CombinedSafetyInvariant                                        *)
(*   PROPERTY PROPERTY_Liveness                                               *)
(*                                                                            *)
================================================================================

================================================================================
(* LICENSE: MIT License                                                       *)
(* Copyright (c) 2026 F1r3fly.io                                              *)
================================================================================
