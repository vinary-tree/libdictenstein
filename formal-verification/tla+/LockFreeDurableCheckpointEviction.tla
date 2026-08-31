---------------- MODULE LockFreeDurableCheckpointEviction ----------------
(***************************************************************************)
(* Composes lock-free semantic-root publication, checkpoint durability,    *)
(* exact eviction-catalog publication, WAL reclamation, and crash recovery. *)
(*                                                                         *)
(* A WAL append does not change semantic root identity. A successful       *)
(* semantic root compare-and-swap advances the revision and atomically      *)
(* clears any exact catalog binding. A checkpoint catalog may become exact *)
(* authority only after its durable image is stamped and only while the    *)
(* captured (revision, generation) pair is still current. Detached callback *)
(* state is advisory and cannot affect exact use or recovery.               *)
(***************************************************************************)

EXTENDS Naturals, FiniteSets, TLC

CONSTANTS
    Writers,
    Lsns,
    NoLsn,
    Generations,
    NoGeneration,
    USE_WATERMARK,
    UnsafeSemanticPreservesBinding,
    UnsafePublishIgnoresRoot,
    UnsafePublishBeforeStamp,
    UnsafeExactUseIgnoresRoot,
    UnsafeRecoveryUsesDetached

ASSUME /\ Writers # {}
       /\ Lsns # {}
       /\ Generations # {}
       /\ NoLsn \notin Lsns
       /\ NoGeneration \notin Generations

WriterPhases == {"Idle", "Appended"}
CheckpointPhases ==
    {"Idle", "Captured", "Durable", "Stamped", "Bound", "Unbound"}
GenerationValues == Generations \cup {NoGeneration}
MaxRevision == Cardinality(Lsns)

VARIABLES
    wphase,
    wlsn,
    appended,
    committed,
    rootRevision,
    rootGeneration,
    ckptPhase,
    ckptSnapshot,
    ckptTarget,
    capturedRevision,
    capturedGeneration,
    candidateGeneration,
    candidateStamped,
    durableCkpt,
    checkpointLsn,
    walRetainedFrom,
    registryRevision,
    registryGeneration,
    registryStamped,
    registryDurableUpTo,
    detachedHint,
    badExactUse,
    recovered,
    recoveryFresh,
    recoveryConsultedDetached

vars ==
    <<wphase, wlsn, appended, committed, rootRevision, rootGeneration,
      ckptPhase, ckptSnapshot, ckptTarget, capturedRevision,
      capturedGeneration, candidateGeneration, candidateStamped,
      durableCkpt, checkpointLsn, walRetainedFrom, registryRevision,
      registryGeneration, registryStamped, registryDurableUpTo,
      detachedHint, badExactUse, recovered, recoveryFresh,
      recoveryConsultedDetached>>

TypeInvariant ==
    /\ wphase \in [Writers -> WriterPhases]
    /\ wlsn \in [Writers -> Lsns \cup {NoLsn}]
    /\ appended \subseteq Lsns
    /\ committed \subseteq appended
    /\ rootRevision \in 0..MaxRevision
    /\ rootGeneration \in GenerationValues
    /\ ckptPhase \in CheckpointPhases
    /\ ckptSnapshot \subseteq Lsns
    /\ ckptTarget \subseteq Lsns
    /\ capturedRevision \in 0..MaxRevision
    /\ capturedGeneration \in GenerationValues
    /\ candidateGeneration \in GenerationValues
    /\ candidateStamped \in BOOLEAN
    /\ durableCkpt \subseteq Lsns
    /\ checkpointLsn \subseteq Lsns
    /\ walRetainedFrom \subseteq appended
    /\ registryRevision \in 0..MaxRevision
    /\ registryGeneration \in GenerationValues
    /\ registryStamped \in BOOLEAN
    /\ registryDurableUpTo \subseteq Lsns
    /\ detachedHint \subseteq Lsns
    /\ badExactUse \in BOOLEAN
    /\ recovered \subseteq Lsns
    /\ recoveryFresh \in BOOLEAN
    /\ recoveryConsultedDetached \in BOOLEAN

RootPairEquals(revision, generation) ==
    rootRevision = revision /\ rootGeneration = generation

ExactAuthority ==
    /\ rootGeneration \in Generations
    /\ registryRevision = rootRevision
    /\ registryGeneration = rootGeneration
    /\ registryStamped

Running == ~recoveryFresh

DurablePrefix == durableCkpt \subseteq committed

RootRevisionTracksCommitted == rootRevision = Cardinality(committed)

ImmutableSnapshotIsClosed ==
    ckptPhase = "Idle" \/ ckptSnapshot \subseteq committed

CaptureEqualsPublishFrontier ==
    ckptPhase \in {"Idle", "Captured"} \/ durableCkpt = ckptSnapshot

NoLostWriteUnderLockFreeCommit ==
    ~recoveryFresh \/ committed \subseteq recovered

RecoveredNeverInventsState ==
    ~recoveryFresh \/ recovered \subseteq committed

ExactRootRegistryAgreement ==
    rootGeneration = NoGeneration \/ ExactAuthority

PublishedCatalogIsStamped ==
    rootGeneration = NoGeneration \/ registryStamped

RegistryPointsAtDurableWatermark ==
    rootGeneration = NoGeneration
    \/ /\ registryDurableUpTo = checkpointLsn
       /\ durableCkpt = ckptSnapshot

NoInexactUse == ~badExactUse

RecoveryIndependentOfDetached == ~recoveryConsultedDetached

Init ==
    /\ wphase = [writer \in Writers |-> "Idle"]
    /\ wlsn = [writer \in Writers |-> NoLsn]
    /\ appended = {}
    /\ committed = {}
    /\ rootRevision = 0
    /\ rootGeneration = NoGeneration
    /\ ckptPhase = "Idle"
    /\ ckptSnapshot = {}
    /\ ckptTarget = {}
    /\ capturedRevision = 0
    /\ capturedGeneration = NoGeneration
    /\ candidateGeneration = NoGeneration
    /\ candidateStamped = FALSE
    /\ durableCkpt = {}
    /\ checkpointLsn = {}
    /\ walRetainedFrom = {}
    /\ registryRevision = 0
    /\ registryGeneration = NoGeneration
    /\ registryStamped = FALSE
    /\ registryDurableUpTo = {}
    /\ detachedHint = Lsns
    /\ badExactUse = FALSE
    /\ recovered = {}
    /\ recoveryFresh = FALSE
    /\ recoveryConsultedDetached = FALSE

Append(writer, lsn) ==
    /\ Running
    /\ writer \in Writers
    /\ lsn \in Lsns \ appended
    /\ wphase[writer] = "Idle"
    /\ wphase' = [wphase EXCEPT ![writer] = "Appended"]
    /\ wlsn' = [wlsn EXCEPT ![writer] = lsn]
    /\ appended' = appended \cup {lsn}
    /\ walRetainedFrom' = walRetainedFrom \cup {lsn}
    /\ UNCHANGED <<committed, rootRevision, rootGeneration, ckptPhase,
                    ckptSnapshot, ckptTarget, capturedRevision,
                    capturedGeneration, candidateGeneration,
                    candidateStamped, durableCkpt, checkpointLsn,
                    registryRevision, registryGeneration, registryStamped,
                    registryDurableUpTo, detachedHint, badExactUse,
                    recovered, recoveryFresh, recoveryConsultedDetached>>

SemanticVisibilityCas(writer) ==
    /\ Running
    /\ writer \in Writers
    /\ wphase[writer] = "Appended"
    /\ wlsn[writer] \notin committed
    /\ wphase' = [wphase EXCEPT ![writer] = "Idle"]
    /\ wlsn' = [wlsn EXCEPT ![writer] = NoLsn]
    /\ committed' = committed \cup {wlsn[writer]}
    /\ rootRevision' = rootRevision + 1
    /\ rootGeneration' =
         IF UnsafeSemanticPreservesBinding
         THEN rootGeneration
         ELSE NoGeneration
    /\ UNCHANGED <<appended, ckptPhase, ckptSnapshot, ckptTarget,
                    capturedRevision, capturedGeneration,
                    candidateGeneration, candidateStamped, durableCkpt,
                    checkpointLsn, walRetainedFrom, registryRevision,
                    registryGeneration, registryStamped,
                    registryDurableUpTo, detachedHint, badExactUse,
                    recovered, recoveryFresh, recoveryConsultedDetached>>

CaptureCheckpoint(generation) ==
    /\ Running
    /\ ckptPhase = "Idle"
    /\ generation \in Generations
    /\ ckptPhase' = "Captured"
    /\ ckptSnapshot' = committed
    /\ ckptTarget' = IF USE_WATERMARK THEN committed ELSE appended
    /\ capturedRevision' = rootRevision
    /\ capturedGeneration' = rootGeneration
    /\ candidateGeneration' = generation
    /\ candidateStamped' = FALSE
    /\ UNCHANGED <<wphase, wlsn, appended, committed, rootRevision,
                    rootGeneration, durableCkpt, checkpointLsn,
                    walRetainedFrom, registryRevision, registryGeneration,
                    registryStamped, registryDurableUpTo, detachedHint,
                    badExactUse, recovered, recoveryFresh,
                    recoveryConsultedDetached>>

PublishDurableImage ==
    /\ Running
    /\ ckptPhase = "Captured"
    /\ ckptPhase' = "Durable"
    /\ durableCkpt' = ckptSnapshot
    /\ checkpointLsn' = ckptTarget
    /\ UNCHANGED <<wphase, wlsn, appended, committed, rootRevision,
                    rootGeneration, ckptSnapshot, ckptTarget,
                    capturedRevision, capturedGeneration,
                    candidateGeneration, candidateStamped, walRetainedFrom,
                    registryRevision, registryGeneration, registryStamped,
                    registryDurableUpTo, detachedHint, badExactUse,
                    recovered, recoveryFresh, recoveryConsultedDetached>>

StampCatalog ==
    /\ Running
    /\ ckptPhase = "Durable"
    /\ ckptPhase' = "Stamped"
    /\ candidateStamped' = TRUE
    /\ UNCHANGED <<wphase, wlsn, appended, committed, rootRevision,
                    rootGeneration, ckptSnapshot, ckptTarget,
                    capturedRevision, capturedGeneration,
                    candidateGeneration, durableCkpt, checkpointLsn,
                    walRetainedFrom, registryRevision, registryGeneration,
                    registryStamped, registryDurableUpTo, detachedHint,
                    badExactUse, recovered, recoveryFresh,
                    recoveryConsultedDetached>>

PublishExactCatalog ==
    /\ Running
    /\ ckptPhase \in {"Durable", "Stamped"}
    /\ candidateGeneration \in Generations
    /\ (candidateStamped \/ UnsafePublishBeforeStamp)
    /\ (RootPairEquals(capturedRevision, capturedGeneration)
        \/ UnsafePublishIgnoresRoot)
    /\ ckptPhase' = "Bound"
    /\ rootGeneration' = candidateGeneration
    /\ registryRevision' = capturedRevision
    /\ registryGeneration' = candidateGeneration
    /\ registryStamped' = candidateStamped
    /\ registryDurableUpTo' = checkpointLsn
    /\ UNCHANGED <<wphase, wlsn, appended, committed, rootRevision,
                    ckptSnapshot, ckptTarget, capturedRevision,
                    capturedGeneration, candidateGeneration,
                    candidateStamped, durableCkpt, checkpointLsn,
                    walRetainedFrom, detachedHint, badExactUse, recovered,
                    recoveryFresh, recoveryConsultedDetached>>

AbortCatalogBinding ==
    /\ Running
    /\ ckptPhase \in {"Durable", "Stamped"}
    /\ ~RootPairEquals(capturedRevision, capturedGeneration)
    /\ ckptPhase' = "Unbound"
    /\ UNCHANGED <<wphase, wlsn, appended, committed, rootRevision,
                    rootGeneration, ckptSnapshot, ckptTarget,
                    capturedRevision, capturedGeneration,
                    candidateGeneration, candidateStamped, durableCkpt,
                    checkpointLsn, walRetainedFrom, registryRevision,
                    registryGeneration, registryStamped,
                    registryDurableUpTo, detachedHint, badExactUse,
                    recovered, recoveryFresh, recoveryConsultedDetached>>

ReclaimWal ==
    /\ Running
    /\ ckptPhase \in {"Durable", "Stamped", "Bound", "Unbound"}
    /\ walRetainedFrom' = walRetainedFrom \ checkpointLsn
    /\ UNCHANGED <<wphase, wlsn, appended, committed, rootRevision,
                    rootGeneration, ckptPhase, ckptSnapshot, ckptTarget,
                    capturedRevision, capturedGeneration,
                    candidateGeneration, candidateStamped, durableCkpt,
                    checkpointLsn, registryRevision, registryGeneration,
                    registryStamped, registryDurableUpTo, detachedHint,
                    badExactUse, recovered, recoveryFresh,
                    recoveryConsultedDetached>>

UseExactCatalog ==
    /\ Running
    /\ (ExactAuthority
        \/ /\ UnsafeExactUseIgnoresRoot
           /\ registryGeneration \in Generations)
    /\ badExactUse' = (badExactUse \/ ~ExactAuthority)
    /\ UNCHANGED <<wphase, wlsn, appended, committed, rootRevision,
                    rootGeneration, ckptPhase, ckptSnapshot, ckptTarget,
                    capturedRevision, capturedGeneration,
                    candidateGeneration, candidateStamped, durableCkpt,
                    checkpointLsn, walRetainedFrom, registryRevision,
                    registryGeneration, registryStamped,
                    registryDurableUpTo, detachedHint, recovered,
                    recoveryFresh, recoveryConsultedDetached>>

CrashRecover ==
    /\ Running
    /\ ckptPhase \in {"Durable", "Stamped", "Bound", "Unbound"}
    /\ recovered' =
         durableCkpt \cup (committed \cap walRetainedFrom)
         \cup IF UnsafeRecoveryUsesDetached THEN detachedHint ELSE {}
    /\ recoveryFresh' = TRUE
    /\ recoveryConsultedDetached' =
         UnsafeRecoveryUsesDetached /\ detachedHint # {}
    /\ rootGeneration' = NoGeneration
    /\ UNCHANGED <<wphase, wlsn, appended, committed, rootRevision,
                    ckptPhase, ckptSnapshot, ckptTarget, capturedRevision,
                    capturedGeneration, candidateGeneration,
                    candidateStamped, durableCkpt, checkpointLsn,
                    walRetainedFrom, registryRevision, registryGeneration,
                    registryStamped, registryDurableUpTo, detachedHint,
                    badExactUse>>

Next ==
    \/ \E writer \in Writers, lsn \in Lsns : Append(writer, lsn)
    \/ \E writer \in Writers : SemanticVisibilityCas(writer)
    \/ \E generation \in Generations : CaptureCheckpoint(generation)
    \/ PublishDurableImage
    \/ StampCatalog
    \/ PublishExactCatalog
    \/ AbortCatalogBinding
    \/ ReclaimWal
    \/ UseExactCatalog
    \/ CrashRecover

Spec == Init /\ [][Next]_vars

=============================================================================
