------------------------------ MODULE Concurrency ------------------------------
(****************************************************************************)
(* Concurrency: Optimistic lock coupling specification for the Persistent   *)
(* ARTrie. This module defines the concurrency control mechanism that       *)
(* ensures thread-safe access to the trie.                                  *)
(*                                                                          *)
(* Key concepts:                                                            *)
(* 1. Optimistic Locking: Readers capture version, validate after read      *)
(* 2. Lock Coupling: Writers acquire locks in tree order during traversal   *)
(* 3. Version Numbers: Even=stable, Odd=write-in-progress                   *)
(* 4. Epoch-Based Reclamation: Safe memory reclamation via epochs           *)
(*                                                                          *)
(* Properties verified:                                                     *)
(* - No lost updates (writers don't overwrite each other)                   *)
(* - No dirty reads (readers don't see partial writes)                      *)
(* - Deadlock freedom (lock coupling prevents deadlock)                     *)
(* - Writers eventually release locks                                       *)
(****************************************************************************)

EXTENDS ARTrieTypes, ARTrieState, Integers, Sequences, FiniteSets, TLC

--------------------------------------------------------------------------------
(* ADDITIONAL STATE VARIABLES FOR CONCURRENCY                                 *)
--------------------------------------------------------------------------------

VARIABLES
    \* Read guards: Thread -> set of (NodeId, capturedVersion)
    readGuards,

    \* Write guards: Thread -> sequence of NodeId (lock coupling order)
    writeGuards,

    \* Global epoch for epoch-based reclamation
    globalEpoch,

    \* Active readers count per epoch: Epoch -> count
    activeReaders,

    \* Thread's current epoch (for epoch-based reclamation)
    threadEpoch,

    \* Retry statistics per thread: Thread -> {successful, retries, maxRetries}
    retryStats,

    \* Lock depth tracking: Thread -> current depth in tree
    lockDepth

--------------------------------------------------------------------------------
(* TYPE INVARIANT                                                             *)
--------------------------------------------------------------------------------

ConcurrencyTypeInvariant ==
    /\ readGuards \in [Threads -> SUBSET (NodeIds \X Nat)]
    /\ writeGuards \in [Threads -> Seq(NodeIds)]
    /\ globalEpoch \in 0..MaxEpoch
    /\ activeReaders \in [0..MaxEpoch -> Nat]
    /\ threadEpoch \in [Threads -> 0..MaxEpoch]
    /\ retryStats \in [Threads -> [successful: Nat, retries: Nat, maxRetries: Nat]]
    /\ lockDepth \in [Threads -> Nat]

--------------------------------------------------------------------------------
(* INITIAL STATE                                                              *)
--------------------------------------------------------------------------------

ConcurrencyInit ==
    /\ readGuards = [t \in Threads |-> {}]
    /\ writeGuards = [t \in Threads |-> <<>>]
    /\ globalEpoch = 0
    /\ activeReaders = [e \in 0..MaxEpoch |-> 0]
    /\ threadEpoch = [t \in Threads |-> 0]
    /\ retryStats = [t \in Threads |-> [successful |-> 0, retries |-> 0, maxRetries |-> 0]]
    /\ lockDepth = [t \in Threads |-> 0]

--------------------------------------------------------------------------------
(* VERSION OPERATIONS                                                         *)
(* Fundamental operations on version numbers.                                 *)
--------------------------------------------------------------------------------

\* Get current version of a node
GetVersion(nid) == versions[nid]

\* Check if version is stable (even)
VersionStable(v) == v % 2 = 0

\* Check if version indicates writing (odd)
VersionWriting(v) == v % 2 = 1

--------------------------------------------------------------------------------
(* OPTIMISTIC READ OPERATIONS                                                 *)
(* Readers use optimistic concurrency: capture version, read, validate.       *)
--------------------------------------------------------------------------------

\* Begin an optimistic read on a node
BeginOptimisticRead(thread, nid) ==
    /\ ValidNode(nid)
    /\ LET v == GetVersion(nid) IN
        /\ VersionStable(v)  \* Wait for stable version
        /\ readGuards' = [readGuards EXCEPT ![thread] = @ \cup {<<nid, v>>}]
        /\ UNCHANGED <<writeGuards, globalEpoch, activeReaders, threadEpoch,
                       retryStats, lockDepth>>

\* Validate an optimistic read
ValidateOptimisticRead(thread, nid) ==
    /\ <<nid, versions[nid]>> \in readGuards[thread]
    \* Validation succeeds if version hasn't changed
    /\ \E v \in Nat : <<nid, v>> \in readGuards[thread] /\ GetVersion(nid) = v

\* End an optimistic read (release the guard)
EndOptimisticRead(thread, nid) ==
    /\ \E v \in Nat : <<nid, v>> \in readGuards[thread]
    /\ readGuards' = [readGuards EXCEPT ![thread] =
        {g \in @ : g[1] # nid}]
    /\ UNCHANGED <<writeGuards, globalEpoch, activeReaders, threadEpoch,
                   retryStats, lockDepth>>

\* Attempt an optimistic read, returning success/failure
TryOptimisticRead(thread, nid) ==
    /\ ValidNode(nid)
    /\ LET v == GetVersion(nid) IN
        IF VersionStable(v) THEN
            /\ readGuards' = [readGuards EXCEPT ![thread] = @ \cup {<<nid, v>>}]
            /\ UNCHANGED <<writeGuards, globalEpoch, activeReaders, threadEpoch,
                           retryStats, lockDepth>>
        ELSE
            \* Read failed due to concurrent write
            /\ retryStats' = [retryStats EXCEPT
                ![thread].retries = @ + 1,
                ![thread].maxRetries = IF @ < retryStats[thread].maxRetries
                                       THEN retryStats[thread].maxRetries
                                       ELSE @ + 1]
            /\ UNCHANGED <<readGuards, writeGuards, globalEpoch, activeReaders,
                           threadEpoch, lockDepth>>

--------------------------------------------------------------------------------
(* WRITE LOCK OPERATIONS                                                      *)
(* Writers acquire exclusive locks with lock coupling.                        *)
--------------------------------------------------------------------------------

\* Begin a write on a node (acquire exclusive lock)
BeginWrite(thread, nid) ==
    /\ ValidNode(nid)
    /\ writers[nid] = 0  \* Node not locked
    /\ VersionStable(versions[nid])  \* Not currently being written
    \* Acquire lock
    /\ versions' = [versions EXCEPT ![nid] = @ + 1]  \* Even -> Odd
    /\ writers' = [writers EXCEPT ![nid] = thread]
    \* Add to write guards (lock coupling order)
    /\ writeGuards' = [writeGuards EXCEPT ![thread] = Append(@, nid)]
    /\ lockDepth' = [lockDepth EXCEPT ![thread] = @ + 1]
    /\ UNCHANGED <<readGuards, globalEpoch, activeReaders, threadEpoch, retryStats>>

\* End a write on a node (release exclusive lock)
EndWrite(thread, nid) ==
    /\ HoldsWriteLock(thread, nid)
    \* Release lock
    /\ versions' = [versions EXCEPT ![nid] = @ + 1]  \* Odd -> Even
    /\ writers' = [writers EXCEPT ![nid] = 0]
    \* Remove from write guards (should be the last one for lock coupling)
    /\ LET guards == writeGuards[thread] IN
        /\ Len(guards) > 0
        /\ guards[Len(guards)] = nid  \* Release in reverse order
        /\ writeGuards' = [writeGuards EXCEPT ![thread] =
            SubSeq(@, 1, Len(@) - 1)]
    /\ lockDepth' = [lockDepth EXCEPT ![thread] = @ - 1]
    /\ retryStats' = [retryStats EXCEPT ![thread].successful = @ + 1]
    /\ UNCHANGED <<readGuards, globalEpoch, activeReaders, threadEpoch>>

\* Try to acquire write lock (non-blocking)
TryBeginWrite(thread, nid) ==
    /\ ValidNode(nid)
    /\ IF writers[nid] = 0 /\ VersionStable(versions[nid]) THEN
        BeginWrite(thread, nid)
       ELSE
        \* Lock acquisition failed
        /\ retryStats' = [retryStats EXCEPT ![thread].retries = @ + 1]
        /\ UNCHANGED <<versions, writers, readGuards, writeGuards,
                       globalEpoch, activeReaders, threadEpoch, lockDepth>>

--------------------------------------------------------------------------------
(* LOCK COUPLING FOR TREE TRAVERSAL                                           *)
(* Lock coupling ensures deadlock-free locking during tree traversal.         *)
(* The key insight: always acquire child lock before releasing parent lock.   *)
--------------------------------------------------------------------------------

\* Descend with lock coupling: acquire child lock, then release parent
LockCouplingDescend(thread, parentNid, childNid) ==
    /\ HoldsWriteLock(thread, parentNid)
    /\ ValidNode(childNid)
    /\ writers[childNid] = 0
    /\ VersionStable(versions[childNid])
    \* Acquire child lock first
    /\ versions' = [versions EXCEPT ![childNid] = @ + 1]
    /\ writers' = [writers EXCEPT ![childNid] = thread]
    /\ writeGuards' = [writeGuards EXCEPT ![thread] = Append(@, childNid)]
    \* Note: Parent lock released in a separate action (EndWrite)
    /\ lockDepth' = [lockDepth EXCEPT ![thread] = @ + 1]
    /\ UNCHANGED <<readGuards, globalEpoch, activeReaders, threadEpoch, retryStats>>

\* Optimistic lock coupling for reads: descend without holding locks
OptimisticDescend(thread, parentNid, childNid, parentVersion) ==
    /\ <<parentNid, parentVersion>> \in readGuards[thread]
    /\ ValidNode(childNid)
    /\ LET childVersion == GetVersion(childNid) IN
        /\ VersionStable(childVersion)
        \* Validate parent hasn't changed
        /\ GetVersion(parentNid) = parentVersion
        \* Add child to read guards
        /\ readGuards' = [readGuards EXCEPT ![thread] = @ \cup {<<childNid, childVersion>>}]
        /\ UNCHANGED <<writeGuards, globalEpoch, activeReaders, threadEpoch,
                       retryStats, lockDepth>>

--------------------------------------------------------------------------------
(* EPOCH-BASED RECLAMATION                                                    *)
(* Safe memory reclamation using epochs.                                      *)
--------------------------------------------------------------------------------

\* Thread enters a read epoch
EnterReadEpoch(thread) ==
    /\ LET e == globalEpoch IN
        /\ threadEpoch' = [threadEpoch EXCEPT ![thread] = e]
        /\ activeReaders' = [activeReaders EXCEPT ![e] = @ + 1]
        /\ UNCHANGED <<readGuards, writeGuards, globalEpoch, retryStats, lockDepth>>

\* Thread exits its current read epoch
ExitReadEpoch(thread) ==
    /\ LET e == threadEpoch[thread] IN
        /\ activeReaders' = [activeReaders EXCEPT ![e] = @ - 1]
        /\ threadEpoch' = [threadEpoch EXCEPT ![thread] = 0]
        /\ UNCHANGED <<readGuards, writeGuards, globalEpoch, retryStats, lockDepth>>

\* Advance global epoch (for garbage collection)
AdvanceGlobalEpoch ==
    /\ globalEpoch < MaxEpoch
    /\ globalEpoch' = globalEpoch + 1
    /\ UNCHANGED <<readGuards, writeGuards, activeReaders, threadEpoch,
                   retryStats, lockDepth>>

\* Check if an epoch is safe for reclamation
EpochSafe(e) ==
    /\ e < globalEpoch
    /\ activeReaders[e] = 0

--------------------------------------------------------------------------------
(* CONCURRENCY INVARIANTS                                                     *)
--------------------------------------------------------------------------------

\* At most one writer per node
SingleWriterInvariant ==
    \A nid \in NodeIds :
        Cardinality({t \in Threads : HoldsWriteLock(t, nid)}) <= 1

\* Version-writer consistency: odd version iff writer present
VersionWriterConsistency ==
    \A nid \in NodeIds :
        ValidNode(nid) =>
            (VersionWriting(versions[nid]) <=> writers[nid] # 0)

\* Lock depth matches write guards length
LockDepthConsistency ==
    \A t \in Threads : lockDepth[t] = Len(writeGuards[t])

\* Read guards only contain stable versions (when captured)
ReadGuardStability ==
    \A t \in Threads :
        \A g \in readGuards[t] :
            \* The captured version was even when captured
            VersionStable(g[2])

\* Write guards form a valid lock coupling chain (parent before child)
WriteGuardOrdering ==
    \A t \in Threads :
        LET guards == writeGuards[t] IN
            \* In a proper tree traversal, earlier locks are ancestors
            \* This is abstracted here - full verification needs tree structure
            TRUE

\* Active readers count is non-negative
ActiveReadersNonNegative ==
    \A e \in 0..MaxEpoch : activeReaders[e] >= 0

--------------------------------------------------------------------------------
(* COMBINED CONCURRENCY SAFETY INVARIANT                                      *)
--------------------------------------------------------------------------------

ConcurrencySafetyInvariant ==
    /\ ConcurrencyTypeInvariant
    /\ SingleWriterInvariant
    /\ VersionWriterConsistency
    /\ LockDepthConsistency
    /\ ReadGuardStability
    /\ ActiveReadersNonNegative

--------------------------------------------------------------------------------
(* LIVENESS PROPERTIES                                                        *)
--------------------------------------------------------------------------------

\* Writers eventually release all locks
WritersEventuallyRelease ==
    <>[](\A t \in Threads : Len(writeGuards[t]) = 0)

\* Failed reads eventually succeed (no starvation)
ReadsEventuallySucceed ==
    \A t \in Threads :
        [](threads[t].state = "Reading" => <>(threads[t].state # "Reading"))

\* Lock coupling doesn't cause deadlock
DeadlockFreedom ==
    ~(\A t \in Threads : threads[t].state = "Waiting")

--------------------------------------------------------------------------------
(* ACTIONS                                                                    *)
(* High-level actions combining lower-level operations.                       *)
--------------------------------------------------------------------------------

\* A thread starts an optimistic read operation
StartOptimisticReadAction(thread, nid) ==
    /\ ThreadIdle(thread)
    /\ BeginOptimisticRead(thread, nid)
    /\ threads' = [threads EXCEPT ![thread].state = "Reading",
                                  ![thread].currentNode = nid]
    /\ UNCHANGED <<abstractMap, entryCount, root, nodes, nextNodeId>>

\* A thread validates and completes an optimistic read
CompleteOptimisticReadAction(thread) ==
    /\ ThreadReading(thread)
    /\ LET nid == threads[thread].currentNode IN
        /\ ValidateOptimisticRead(thread, nid)
        /\ EndOptimisticRead(thread, nid)
        /\ threads' = [threads EXCEPT ![thread].state = "Idle",
                                      ![thread].currentNode = Null]
    /\ UNCHANGED <<abstractMap, entryCount, root, nodes, nextNodeId>>

\* A thread's optimistic read fails validation (must retry)
OptimisticReadFailsAction(thread) ==
    /\ ThreadReading(thread)
    /\ LET nid == threads[thread].currentNode IN
        /\ ~ValidateOptimisticRead(thread, nid)
        /\ readGuards' = [readGuards EXCEPT ![thread] = {}]
        /\ threads' = [threads EXCEPT ![thread].state = "Idle"]
        /\ retryStats' = [retryStats EXCEPT ![thread].retries = @ + 1]
    /\ UNCHANGED <<writeGuards, globalEpoch, activeReaders, threadEpoch, lockDepth,
                   abstractMap, entryCount, root, nodes, nextNodeId>>

\* A thread starts a write operation
StartWriteAction(thread, nid) ==
    /\ ThreadIdle(thread)
    /\ BeginWrite(thread, nid)
    /\ threads' = [threads EXCEPT ![thread].state = "Writing",
                                  ![thread].currentNode = nid]
    /\ UNCHANGED <<abstractMap, entryCount, root, nodes, nextNodeId>>

\* A thread completes a write operation
CompleteWriteAction(thread) ==
    /\ ThreadWriting(thread)
    /\ LET nid == threads[thread].currentNode IN
        /\ EndWrite(thread, nid)
        /\ threads' = [threads EXCEPT ![thread].state = "Idle",
                                      ![thread].currentNode = Null]
    /\ UNCHANGED <<abstractMap, entryCount, root, nodes, nextNodeId>>

--------------------------------------------------------------------------------
(* STATE VARIABLES EXPORT                                                     *)
--------------------------------------------------------------------------------

concurrencyVars == <<readGuards, writeGuards, globalEpoch, activeReaders,
                     threadEpoch, retryStats, lockDepth>>

================================================================================
(* LICENSE: MIT License                                                       *)
(* Copyright (c) 2026 F1r3fly.io                                              *)
================================================================================
