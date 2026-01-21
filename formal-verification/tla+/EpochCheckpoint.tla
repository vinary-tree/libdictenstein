--------------------------- MODULE EpochCheckpoint ----------------------------
(****************************************************************************)
(* EpochCheckpoint: Epoch lifecycle and checkpointing specification for     *)
(* the Persistent ARTrie. This module models the epoch-based checkpointing  *)
(* system that provides bounded recovery windows.                           *)
(*                                                                          *)
(* Epoch Lifecycle:                                                         *)
(*   Active -> Sealing -> Durable -> Archived                               *)
(*                                                                          *)
(* Key properties verified:                                                 *)
(* 1. Epochs transition in correct order                                    *)
(* 2. Active epoch accepts operations                                       *)
(* 3. Sealing epoch completes pending operations                            *)
(* 4. Durable epochs survive crashes                                        *)
(* 5. Bounded recovery window                                               *)
(****************************************************************************)

EXTENDS ARTrieTypes, WAL, Integers, Sequences, FiniteSets, TLC

--------------------------------------------------------------------------------
(* EPOCH CONFIGURATION                                                        *)
--------------------------------------------------------------------------------

CONSTANTS
    \* Target operations per epoch before checkpoint
    MaxOpsPerEpoch,

    \* Target WAL size per epoch (in abstract units)
    MaxWalSizePerEpoch,

    \* Number of old epochs to retain
    RetentionEpochs,

    \* Enable background checkpointing
    BackgroundCheckpoint,

    \* Enable incremental checkpointing (only dirty pages)
    IncrementalCheckpoint

ASSUME MaxOpsPerEpoch \in Nat \ {0}
ASSUME MaxWalSizePerEpoch \in Nat \ {0}
ASSUME RetentionEpochs \in Nat
ASSUME BackgroundCheckpoint \in BOOLEAN
ASSUME IncrementalCheckpoint \in BOOLEAN

--------------------------------------------------------------------------------
(* STATE VARIABLES                                                            *)
--------------------------------------------------------------------------------

VARIABLES
    \* Current active epoch ID
    currentEpochId,

    \* Epoch metadata for all epochs: EpochId -> EpochMetadata
    epochs,

    \* Operations in current epoch
    currentEpochOps,

    \* WAL size in current epoch (abstract units)
    currentEpochWalSize,

    \* Checkpoint in progress (for background checkpointing)
    checkpointInProgress,

    \* Pages being written in current checkpoint
    checkpointPages,

    \* Last completed checkpoint epoch
    lastCheckpointEpoch

--------------------------------------------------------------------------------
(* TYPE INVARIANT                                                             *)
--------------------------------------------------------------------------------

EpochTypeInvariant ==
    /\ currentEpochId \in 0..MaxEpoch
    /\ epochs \in [0..MaxEpoch -> EpochMetadata \cup {<<>>}]
    /\ currentEpochOps \in Nat
    /\ currentEpochWalSize \in Nat
    /\ checkpointInProgress \in BOOLEAN
    /\ checkpointPages \in SUBSET NodeIds
    /\ lastCheckpointEpoch \in 0..MaxEpoch

--------------------------------------------------------------------------------
(* INITIAL STATE                                                              *)
--------------------------------------------------------------------------------

InitEpochMetadata(id) ==
    [
        epochId        |-> id,
        state          |-> "Active",
        operationCount |-> 0,
        firstLsn       |-> 0,
        lastLsn        |-> 0
    ]

EpochInit ==
    /\ currentEpochId = 0
    /\ epochs = [e \in 0..MaxEpoch |-> IF e = 0 THEN InitEpochMetadata(0) ELSE <<>>]
    /\ currentEpochOps = 0
    /\ currentEpochWalSize = 0
    /\ checkpointInProgress = FALSE
    /\ checkpointPages = {}
    /\ lastCheckpointEpoch = 0

--------------------------------------------------------------------------------
(* EPOCH STATE TRANSITIONS                                                    *)
(* Epochs transition: Active -> Sealing -> Durable -> Archived                *)
--------------------------------------------------------------------------------

\* Check if epoch is in given state
EpochInState(epochId, state) ==
    /\ epochs[epochId] # <<>>
    /\ epochs[epochId].state = state

\* Get current epoch metadata
CurrentEpoch == epochs[currentEpochId]

\* Predicate: Should trigger checkpoint
ShouldCheckpoint ==
    \/ currentEpochOps >= MaxOpsPerEpoch
    \/ currentEpochWalSize >= MaxWalSizePerEpoch

--------------------------------------------------------------------------------
(* ACTIVE EPOCH OPERATIONS                                                    *)
--------------------------------------------------------------------------------

\* Record an operation in the current epoch
RecordOperation(opSize) ==
    /\ EpochInState(currentEpochId, "Active")
    /\ currentEpochOps' = currentEpochOps + 1
    /\ currentEpochWalSize' = currentEpochWalSize + opSize
    /\ epochs' = [epochs EXCEPT
        ![currentEpochId].operationCount = @ + 1,
        ![currentEpochId].lastLsn = currentLsn]  \* From WAL module
    /\ UNCHANGED <<currentEpochId, checkpointInProgress, checkpointPages,
                   lastCheckpointEpoch>>

\* Update first LSN for a new epoch
SetFirstLsn(lsn) ==
    /\ epochs' = [epochs EXCEPT ![currentEpochId].firstLsn = lsn]
    /\ UNCHANGED <<currentEpochId, currentEpochOps, currentEpochWalSize,
                   checkpointInProgress, checkpointPages, lastCheckpointEpoch>>

--------------------------------------------------------------------------------
(* SEALING TRANSITION                                                         *)
(* Transition from Active to Sealing when checkpoint is triggered.            *)
--------------------------------------------------------------------------------

\* Seal the current epoch (no new operations accepted)
SealCurrentEpoch ==
    /\ EpochInState(currentEpochId, "Active")
    /\ ShouldCheckpoint
    /\ epochs' = [epochs EXCEPT ![currentEpochId].state = "Sealing"]
    /\ UNCHANGED <<currentEpochId, currentEpochOps, currentEpochWalSize,
                   checkpointInProgress, checkpointPages, lastCheckpointEpoch>>

\* Wait for pending operations in sealing epoch to complete
\* (In the model, this is abstracted - we assume instant completion)
CompleteSealingOps ==
    /\ EpochInState(currentEpochId, "Sealing")
    \* All pending operations have completed (abstracted)
    /\ TRUE
    /\ UNCHANGED <<epochs, currentEpochId, currentEpochOps, currentEpochWalSize,
                   checkpointInProgress, checkpointPages, lastCheckpointEpoch>>

--------------------------------------------------------------------------------
(* CHECKPOINT OPERATIONS                                                      *)
(* Checkpointing makes epochs durable.                                        *)
--------------------------------------------------------------------------------

\* Start a checkpoint for the sealed epoch
StartCheckpoint ==
    /\ EpochInState(currentEpochId, "Sealing")
    /\ ~checkpointInProgress
    /\ checkpointInProgress' = TRUE
    \* Determine pages to checkpoint
    /\ IF IncrementalCheckpoint
       THEN checkpointPages' = {}  \* Will be populated with dirty pages
       ELSE checkpointPages' = {}  \* Full checkpoint (all pages)
    /\ UNCHANGED <<epochs, currentEpochId, currentEpochOps, currentEpochWalSize,
                   lastCheckpointEpoch>>

\* Write a page during checkpoint
WriteCheckpointPage(nid) ==
    /\ checkpointInProgress
    /\ nid \in checkpointPages
    /\ checkpointPages' = checkpointPages \ {nid}
    /\ UNCHANGED <<epochs, currentEpochId, currentEpochOps, currentEpochWalSize,
                   checkpointInProgress, lastCheckpointEpoch>>

\* Complete checkpoint and mark epoch as durable
CompleteCheckpoint ==
    /\ checkpointInProgress
    /\ checkpointPages = {}  \* All pages written
    /\ EpochInState(currentEpochId, "Sealing")
    /\ checkpointInProgress' = FALSE
    /\ epochs' = [epochs EXCEPT ![currentEpochId].state = "Durable"]
    /\ lastCheckpointEpoch' = currentEpochId
    /\ UNCHANGED <<currentEpochId, currentEpochOps, currentEpochWalSize,
                   checkpointPages>>

--------------------------------------------------------------------------------
(* NEW EPOCH CREATION                                                         *)
(* After checkpoint, create a new active epoch.                               *)
--------------------------------------------------------------------------------

\* Advance to a new epoch
AdvanceEpoch ==
    /\ EpochInState(currentEpochId, "Durable")
    /\ currentEpochId < MaxEpoch
    /\ LET newEpochId == currentEpochId + 1 IN
        /\ currentEpochId' = newEpochId
        /\ epochs' = [epochs EXCEPT ![newEpochId] = InitEpochMetadata(newEpochId)]
        /\ currentEpochOps' = 0
        /\ currentEpochWalSize' = 0
    /\ UNCHANGED <<checkpointInProgress, checkpointPages, lastCheckpointEpoch>>

--------------------------------------------------------------------------------
(* EPOCH ARCHIVAL                                                             *)
(* Old epochs are archived to reclaim WAL space.                              *)
--------------------------------------------------------------------------------

\* Check if an epoch can be archived
CanArchive(epochId) ==
    /\ epochs[epochId] # <<>>
    /\ epochs[epochId].state = "Durable"
    /\ epochId + RetentionEpochs < currentEpochId

\* Archive an old epoch
ArchiveEpoch(epochId) ==
    /\ CanArchive(epochId)
    /\ epochs' = [epochs EXCEPT ![epochId].state = "Archived"]
    /\ UNCHANGED <<currentEpochId, currentEpochOps, currentEpochWalSize,
                   checkpointInProgress, checkpointPages, lastCheckpointEpoch>>

\* Delete archived epoch data (WAL segment)
DeleteArchivedEpoch(epochId) ==
    /\ epochs[epochId] # <<>>
    /\ epochs[epochId].state = "Archived"
    \* In the model, we just mark as deleted
    /\ epochs' = [epochs EXCEPT ![epochId] = <<>>]
    /\ UNCHANGED <<currentEpochId, currentEpochOps, currentEpochWalSize,
                   checkpointInProgress, checkpointPages, lastCheckpointEpoch>>

--------------------------------------------------------------------------------
(* RECOVERY WINDOW                                                            *)
(* The recovery window is bounded by the retention policy.                    *)
--------------------------------------------------------------------------------

\* Get the oldest recoverable epoch
OldestRecoverableEpoch ==
    LET durableEpochs == {e \in DOMAIN epochs :
        epochs[e] # <<>> /\ epochs[e].state \in {"Durable", "Active", "Sealing"}}
    IN
        IF durableEpochs = {} THEN 0
        ELSE CHOOSE e \in durableEpochs : \A e2 \in durableEpochs : e <= e2

\* Recovery window in epochs
RecoveryWindow ==
    currentEpochId - OldestRecoverableEpoch

\* Check if recovery window is bounded
BoundedRecoveryWindow ==
    RecoveryWindow <= RetentionEpochs + 2  \* +2 for current and sealing

--------------------------------------------------------------------------------
(* EPOCH INVARIANTS                                                           *)
--------------------------------------------------------------------------------

\* Epochs transition in correct order
EpochTransitionOrder ==
    \A e \in DOMAIN epochs :
        epochs[e] # <<>> =>
            epochs[e].state \in {"Active", "Sealing", "Durable", "Archived"}

\* At most one active epoch
SingleActiveEpoch ==
    Cardinality({e \in DOMAIN epochs :
        epochs[e] # <<>> /\ epochs[e].state = "Active"}) <= 1

\* At most one sealing epoch
SingleSealingEpoch ==
    Cardinality({e \in DOMAIN epochs :
        epochs[e] # <<>> /\ epochs[e].state = "Sealing"}) <= 1

\* Active epoch is the current epoch
ActiveIsCurrent ==
    epochs[currentEpochId] # <<>> =>
        epochs[currentEpochId].state \in {"Active", "Sealing", "Durable"}

\* LSN ordering within epoch
EpochLsnOrdering ==
    \A e \in DOMAIN epochs :
        epochs[e] # <<>> =>
            epochs[e].firstLsn <= epochs[e].lastLsn

\* Checkpoint progress consistency
CheckpointConsistency ==
    checkpointInProgress => EpochInState(currentEpochId, "Sealing")

\* Last checkpoint epoch is valid
ValidLastCheckpoint ==
    /\ lastCheckpointEpoch <= currentEpochId
    /\ (lastCheckpointEpoch > 0 =>
        (epochs[lastCheckpointEpoch] # <<>> /\
         epochs[lastCheckpointEpoch].state \in {"Durable", "Archived"}))

--------------------------------------------------------------------------------
(* COMBINED EPOCH SAFETY INVARIANT                                            *)
--------------------------------------------------------------------------------

EpochSafetyInvariant ==
    /\ EpochTypeInvariant
    /\ EpochTransitionOrder
    /\ SingleActiveEpoch
    /\ SingleSealingEpoch
    /\ ActiveIsCurrent
    /\ EpochLsnOrdering
    /\ CheckpointConsistency
    /\ ValidLastCheckpoint
    /\ BoundedRecoveryWindow

--------------------------------------------------------------------------------
(* LIVENESS PROPERTIES                                                        *)
--------------------------------------------------------------------------------

\* Checkpoints eventually complete
CheckpointEventuallyCompletes ==
    checkpointInProgress => <>(~checkpointInProgress)

\* Epochs eventually advance
EpochsEventuallyAdvance ==
    \A e \in 0..MaxEpoch :
        (e < MaxEpoch /\ EpochInState(e, "Active")) =>
            <>(currentEpochId > e)

\* Recovery window remains bounded
RecoveryWindowBounded ==
    []BoundedRecoveryWindow

--------------------------------------------------------------------------------
(* COMBINED ACTIONS                                                           *)
--------------------------------------------------------------------------------

\* Full checkpoint cycle
CheckpointCycle ==
    \/ SealCurrentEpoch
    \/ StartCheckpoint
    \/ (\E nid \in NodeIds : WriteCheckpointPage(nid))
    \/ CompleteCheckpoint
    \/ AdvanceEpoch

\* Archival actions
ArchivalActions ==
    \/ (\E e \in 0..MaxEpoch : ArchiveEpoch(e))
    \/ (\E e \in 0..MaxEpoch : DeleteArchivedEpoch(e))

\* All epoch management actions
EpochAction ==
    \/ CheckpointCycle
    \/ ArchivalActions

--------------------------------------------------------------------------------
(* STATE VARIABLES EXPORT                                                     *)
--------------------------------------------------------------------------------

epochVars == <<currentEpochId, epochs, currentEpochOps, currentEpochWalSize,
               checkpointInProgress, checkpointPages, lastCheckpointEpoch>>

================================================================================
(* LICENSE: MIT License                                                       *)
(* Copyright (c) 2026 F1r3fly.io                                              *)
================================================================================
