-------------------------- MODULE MmapBlockStorage --------------------------
(****************************************************************************)
(* Bounded mmap block-storage synchronization model.                         *)
(*                                                                          *)
(* Scope: concurrent allocation publishes block_count before the backing     *)
(* file and mmap length are extended. Readers/writers must acquire the mmap  *)
(* lock and check the published file_size before touching mapped memory.     *)
(****************************************************************************)

EXTENDS Naturals, FiniteSets, TLC

CONSTANTS Threads, MaxBlock, None

VARIABLES blockCount, actualSize, fileSize, mmapLen, lockOwner,
          phase, claim, allocated,
          successfulReads, failedReads, successfulWrites, failedWrites

Vars == <<blockCount, actualSize, fileSize, mmapLen, lockOwner,
          phase, claim, allocated,
          successfulReads, failedReads, successfulWrites, failedWrites>>

Blocks == 0..(MaxBlock - 1)
Phases == {"Idle", "Claimed", "Locked", "Extended", "Remapped", "Published"}

Max(a, b) == IF a >= b THEN a ELSE b

TypeInvariant ==
    /\ blockCount \in 1..MaxBlock
    /\ actualSize \in 1..MaxBlock
    /\ fileSize \in 1..MaxBlock
    /\ mmapLen \in 1..MaxBlock
    /\ lockOwner \in Threads \cup {None}
    /\ phase \in [Threads -> Phases]
    /\ claim \in [Threads -> Blocks \cup {None}]
    /\ allocated \in SUBSET Blocks
    /\ successfulReads \in SUBSET Blocks
    /\ failedReads \in SUBSET Blocks
    /\ successfulWrites \in SUBSET Blocks
    /\ failedWrites \in SUBSET Blocks

Init ==
    /\ blockCount = 1
    /\ actualSize = 1
    /\ fileSize = 1
    /\ mmapLen = 1
    /\ lockOwner = None
    /\ phase = [t \in Threads |-> "Idle"]
    /\ claim = [t \in Threads |-> None]
    /\ allocated = {0}
    /\ successfulReads = {}
    /\ failedReads = {}
    /\ successfulWrites = {}
    /\ failedWrites = {}

StartAlloc(t) ==
    /\ t \in Threads
    /\ phase[t] = "Idle"
    /\ blockCount < MaxBlock
    /\ claim' = [claim EXCEPT ![t] = blockCount]
    /\ phase' = [phase EXCEPT ![t] = "Claimed"]
    /\ blockCount' = blockCount + 1
    /\ UNCHANGED <<actualSize, fileSize, mmapLen, lockOwner, allocated,
                  successfulReads, failedReads, successfulWrites, failedWrites>>

AcquireAllocLock(t) ==
    /\ t \in Threads
    /\ phase[t] = "Claimed"
    /\ lockOwner = None
    /\ lockOwner' = t
    /\ phase' = [phase EXCEPT ![t] = "Locked"]
    /\ UNCHANGED <<blockCount, actualSize, fileSize, mmapLen, claim, allocated,
                  successfulReads, failedReads, successfulWrites, failedWrites>>

ExtendFile(t) ==
    /\ t \in Threads
    /\ phase[t] = "Locked"
    /\ lockOwner = t
    /\ actualSize' = Max(actualSize, claim[t] + 1)
    /\ phase' = [phase EXCEPT ![t] = "Extended"]
    /\ UNCHANGED <<blockCount, fileSize, mmapLen, lockOwner, claim, allocated,
                  successfulReads, failedReads, successfulWrites, failedWrites>>

RemapMmap(t) ==
    /\ t \in Threads
    /\ phase[t] = "Extended"
    /\ lockOwner = t
    /\ mmapLen' = Max(mmapLen, actualSize)
    /\ phase' = [phase EXCEPT ![t] = "Remapped"]
    /\ UNCHANGED <<blockCount, actualSize, fileSize, lockOwner, claim, allocated,
                  successfulReads, failedReads, successfulWrites, failedWrites>>

PublishFileSize(t) ==
    /\ t \in Threads
    /\ phase[t] = "Remapped"
    /\ lockOwner = t
    /\ fileSize' = Max(fileSize, mmapLen)
    /\ phase' = [phase EXCEPT ![t] = "Published"]
    /\ UNCHANGED <<blockCount, actualSize, mmapLen, lockOwner, claim, allocated,
                  successfulReads, failedReads, successfulWrites, failedWrites>>

CompleteAlloc(t) ==
    /\ t \in Threads
    /\ phase[t] = "Published"
    /\ lockOwner = t
    /\ allocated' = allocated \cup {claim[t]}
    /\ claim' = [claim EXCEPT ![t] = None]
    /\ phase' = [phase EXCEPT ![t] = "Idle"]
    /\ lockOwner' = None
    /\ UNCHANGED <<blockCount, actualSize, fileSize, mmapLen,
                  successfulReads, failedReads, successfulWrites, failedWrites>>

ReadBlock(t, b) ==
    /\ t \in Threads
    /\ b \in Blocks
    /\ phase[t] = "Idle"
    /\ lockOwner = None
    /\ \/ /\ b < blockCount
          /\ b + 1 <= fileSize
          /\ successfulReads' = successfulReads \cup {b}
          /\ failedReads' = failedReads
       \/ /\ ~(b < blockCount /\ b + 1 <= fileSize)
          /\ successfulReads' = successfulReads
          /\ failedReads' = failedReads \cup {b}
    /\ UNCHANGED <<blockCount, actualSize, fileSize, mmapLen, lockOwner,
                  phase, claim, allocated, successfulWrites, failedWrites>>

WriteBlock(t, b) ==
    /\ t \in Threads
    /\ b \in Blocks
    /\ phase[t] = "Idle"
    /\ lockOwner = None
    /\ \/ /\ b < blockCount
          /\ b + 1 <= fileSize
          /\ successfulWrites' = successfulWrites \cup {b}
          /\ failedWrites' = failedWrites
       \/ /\ ~(b < blockCount /\ b + 1 <= fileSize)
          /\ successfulWrites' = successfulWrites
          /\ failedWrites' = failedWrites \cup {b}
    /\ UNCHANGED <<blockCount, actualSize, fileSize, mmapLen, lockOwner,
                  phase, claim, allocated, successfulReads, failedReads>>

Next ==
    \/ \E t \in Threads : StartAlloc(t)
    \/ \E t \in Threads : AcquireAllocLock(t)
    \/ \E t \in Threads : ExtendFile(t)
    \/ \E t \in Threads : RemapMmap(t)
    \/ \E t \in Threads : PublishFileSize(t)
    \/ \E t \in Threads : CompleteAlloc(t)
    \/ \E t \in Threads, b \in Blocks : ReadBlock(t, b)
    \/ \E t \in Threads, b \in Blocks : WriteBlock(t, b)

MmapCoversPublishedFileSize ==
    fileSize <= mmapLen

PublishedFileDoesNotExceedActualFile ==
    fileSize <= actualSize

SuccessfulAccessWithinMmap ==
    \A b \in successfulReads \cup successfulWrites :
        /\ b < blockCount
        /\ b + 1 <= fileSize
        /\ b + 1 <= mmapLen

NoDuplicateActiveClaims ==
    \A t1 \in Threads :
        \A t2 \in Threads :
            t1 # t2 /\ claim[t1] # None /\ claim[t2] # None =>
                claim[t1] # claim[t2]

CompletedAllocationsArePublished ==
    \A b \in allocated :
        /\ b < blockCount
        /\ b + 1 <= fileSize
        /\ b + 1 <= mmapLen

LockHeldOnlyDuringAllocation ==
    lockOwner # None =>
        phase[lockOwner] \in {"Locked", "Extended", "Remapped", "Published"}

Spec == Init /\ [][Next]_Vars

=============================================================================
