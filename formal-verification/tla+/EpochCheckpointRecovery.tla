---------------------- MODULE EpochCheckpointRecovery ----------------------
(***************************************************************************)
(* Focused epoch checkpoint/recovery model for PersistentARTrieChar.        *)
(*                                                                         *)
(* This model covers the implementation correspondence that matters for     *)
(* forced epoch checkpoints: ordinary public mutations first append WAL      *)
(* records, a forced epoch checkpoint publishes the trie data checkpoint     *)
(* before epoch metadata, and WAL cleanup never removes the only recovery   *)
(* evidence for a visible operation.                                        *)
(***************************************************************************)

EXTENDS Naturals, FiniteSets, TLC

CONSTANTS
    MaxEpoch,
    MaxOps,
    RetentionEpochs

ASSUME MaxEpoch \in Nat \ {0}
ASSUME MaxOps \in Nat \ {0}
ASSUME RetentionEpochs \in Nat

Epochs == 0..MaxEpoch
Ops == 1..MaxOps
NoEpoch == MaxEpoch + 1
MaybeEpoch == Epochs \cup {NoEpoch}

VARIABLES
    \* Current accepting epoch.
    currentEpoch,

    \* Operations that have become visible in the trie.
    visibleOps,

    \* Operations covered by the durable trie checkpoint.
    checkpointedOps,

    \* Operation-to-epoch assignment for WAL replay.
    opEpoch,

    \* Epoch WAL segments still available for replay.
    retainedWal,

    \* Last epoch for which metadata was durably published.
    lastDurableEpoch,

    \* The trie data checkpoint has completed but epoch metadata has not.
    dataCheckpointPending,
    pendingEpoch

vars == <<currentEpoch, visibleOps, checkpointedOps, opEpoch, retainedWal,
          lastDurableEpoch, dataCheckpointPending, pendingEpoch>>

Init ==
    /\ currentEpoch = 0
    /\ visibleOps = {}
    /\ checkpointedOps = {}
    /\ opEpoch = [o \in Ops |-> NoEpoch]
    /\ retainedWal = {0}
    /\ lastDurableEpoch = NoEpoch
    /\ dataCheckpointPending = FALSE
    /\ pendingEpoch = NoEpoch

Cleanup(wals, durableEpoch) ==
    IF durableEpoch = NoEpoch
    THEN wals
    ELSE {e \in wals : e + RetentionEpochs >= durableEpoch}

RecoverableOps ==
    checkpointedOps \cup {o \in visibleOps : opEpoch[o] \in retainedWal}

OpsThroughEpoch(e) ==
    {o \in visibleOps : opEpoch[o] # NoEpoch /\ opEpoch[o] <= e}

AppendOp(o) ==
    /\ ~dataCheckpointPending
    /\ o \in Ops \ visibleOps
    /\ visibleOps' = visibleOps \cup {o}
    /\ opEpoch' = [opEpoch EXCEPT ![o] = currentEpoch]
    /\ retainedWal' = retainedWal \cup {currentEpoch}
    /\ UNCHANGED <<currentEpoch, checkpointedOps, lastDurableEpoch,
                   dataCheckpointPending, pendingEpoch>>

\* Models threshold-driven epoch advancement. This rotates epoch metadata and
\* WAL segments but does not publish durable epoch metadata.
AdvanceWithoutDataCheckpoint ==
    /\ ~dataCheckpointPending
    /\ currentEpoch < MaxEpoch
    /\ currentEpoch' = currentEpoch + 1
    /\ retainedWal' = retainedWal \cup {currentEpoch, currentEpoch + 1}
    /\ UNCHANGED <<visibleOps, checkpointedOps, opEpoch, lastDurableEpoch,
                   dataCheckpointPending, pendingEpoch>>

\* Forced checkpoints first persist and verify the trie data checkpoint.
StartForceCheckpoint ==
    /\ ~dataCheckpointPending
    /\ currentEpoch < MaxEpoch
    /\ checkpointedOps' = visibleOps
    /\ dataCheckpointPending' = TRUE
    /\ pendingEpoch' = currentEpoch
    /\ UNCHANGED <<currentEpoch, visibleOps, opEpoch, retainedWal,
                   lastDurableEpoch>>

\* Epoch metadata can be published only after the data checkpoint step.
PublishEpochMetadata ==
    /\ dataCheckpointPending
    /\ pendingEpoch = currentEpoch
    /\ currentEpoch < MaxEpoch
    /\ currentEpoch' = currentEpoch + 1
    /\ lastDurableEpoch' = pendingEpoch
    /\ retainedWal' = Cleanup(retainedWal \cup {currentEpoch, currentEpoch + 1},
                              pendingEpoch)
    /\ dataCheckpointPending' = FALSE
    /\ pendingEpoch' = NoEpoch
    /\ UNCHANGED <<visibleOps, checkpointedOps, opEpoch>>

\* Metadata publication can fail after the trie checkpoint. It must not create
\* a new durable-epoch claim.
FailEpochMetadataBeforeAdvance ==
    /\ dataCheckpointPending
    /\ dataCheckpointPending' = FALSE
    /\ pendingEpoch' = NoEpoch
    /\ UNCHANGED <<currentEpoch, visibleOps, checkpointedOps, opEpoch,
                   retainedWal, lastDurableEpoch>>

FailEpochMetadataAfterAdvance ==
    /\ dataCheckpointPending
    /\ pendingEpoch = currentEpoch
    /\ currentEpoch < MaxEpoch
    /\ currentEpoch' = currentEpoch + 1
    /\ retainedWal' = retainedWal \cup {currentEpoch, currentEpoch + 1}
    /\ dataCheckpointPending' = FALSE
    /\ pendingEpoch' = NoEpoch
    /\ UNCHANGED <<visibleOps, checkpointedOps, opEpoch, lastDurableEpoch>>

\* Optional cleanup is permitted only for WAL epochs whose visible operations
\* are already covered by the trie checkpoint.
CleanupRetainedWal(e) ==
    /\ e \in retainedWal
    /\ lastDurableEpoch # NoEpoch
    /\ e + RetentionEpochs < lastDurableEpoch
    /\ \A o \in visibleOps : opEpoch[o] = e => o \in checkpointedOps
    /\ retainedWal' = retainedWal \ {e}
    /\ UNCHANGED <<currentEpoch, visibleOps, checkpointedOps, opEpoch,
                   lastDurableEpoch, dataCheckpointPending, pendingEpoch>>

Next ==
    \/ \E o \in Ops : AppendOp(o)
    \/ AdvanceWithoutDataCheckpoint
    \/ StartForceCheckpoint
    \/ PublishEpochMetadata
    \/ FailEpochMetadataBeforeAdvance
    \/ FailEpochMetadataAfterAdvance
    \/ \E e \in Epochs : CleanupRetainedWal(e)

TypeInvariant ==
    /\ currentEpoch \in Epochs
    /\ visibleOps \subseteq Ops
    /\ checkpointedOps \subseteq visibleOps
    /\ opEpoch \in [Ops -> MaybeEpoch]
    /\ retainedWal \subseteq Epochs
    /\ currentEpoch \in retainedWal
    /\ lastDurableEpoch \in MaybeEpoch
    /\ dataCheckpointPending \in BOOLEAN
    /\ pendingEpoch \in MaybeEpoch
    /\ dataCheckpointPending => pendingEpoch = currentEpoch
    /\ ~dataCheckpointPending => pendingEpoch = NoEpoch
    /\ \A o \in Ops : o \notin visibleOps => opEpoch[o] = NoEpoch
    /\ \A o \in visibleOps : opEpoch[o] \in Epochs /\ opEpoch[o] <= currentEpoch

RecoveryCoversVisible ==
    visibleOps \subseteq RecoverableOps

UncheckpointedOpsKeepWal ==
    \A o \in visibleOps \ checkpointedOps : opEpoch[o] \in retainedWal

CheckpointCoversDurableEpoch ==
    lastDurableEpoch = NoEpoch \/
        OpsThroughEpoch(lastDurableEpoch) \subseteq checkpointedOps

NoDurableOverclaim ==
    lastDurableEpoch = NoEpoch \/ lastDurableEpoch < currentEpoch

Spec == Init /\ [][Next]_vars

=============================================================================
