---------------- MODULE EvictionExactRootPublication ----------------
(***************************************************************************)
(* Exact-root eviction authority without semantic-writer or detached-       *)
(* callback exclusion. A semantic writer may remain WAL-durable while a      *)
(* checkpoint prepares: the existing root compare-and-swap orders semantic   *)
(* visibility against exact registry publication. Semantic successors clear   *)
(* the root binding in that same atomic transition.                           *)
(*                                                                           *)
(* Detached compatibility callbacks are modeled separately by                *)
(* DetachedCallbackSeparation.tla. They retain immutable advisory snapshots   *)
(* and never participate in this exact-root protocol.                         *)
(***************************************************************************)

EXTENDS Naturals, FiniteSets, TLC

CONSTANTS
    WriterIds,
    Revisions,
    Generations,
    UnsafeSemanticPreservesBinding,
    UnsafeExactCommitIgnoresRoot,
    UnsafePublishBeforeStamp,
    UnsafeRetireKeepsBinding,
    UnsafeReuseRetainedGeneration,
    UnsafeFailedPublishKeepsRegistry

ASSUME /\ WriterIds # {}
       /\ Revisions # {}
       /\ Generations # {}

NoGeneration == "NoGeneration"
NoRevision == "NoRevision"
GenerationValues == Generations \cup {NoGeneration}
RevisionValues == Revisions \cup {NoRevision}
WriterPhases == {"Idle", "WalDurable"}
CheckpointPhases == {"Idle", "Prepared", "Stamped"}
BatchPhases == {"Idle", "Selected"}

VARIABLES
    rootRevision,
    rootGeneration,
    registryRevision,
    registryGeneration,
    registryStamped,
    retired,
    writerPhase,
    writerObservedRevision,
    writerObservedGeneration,
    writerTargetRevision,
    checkpointPhase,
    capturedRevision,
    capturedGeneration,
    candidateGeneration,
    candidateStamped,
    batchPhase,
    batchRevision,
    batchGeneration,
    publicationClaim,
    usedGenerations,
    badExactCommit,
    badGenerationReuse,
    badFailedRollback

vars == <<rootRevision, rootGeneration, registryRevision,
          registryGeneration, registryStamped, retired, writerPhase,
          writerObservedRevision, writerObservedGeneration,
          writerTargetRevision, checkpointPhase, capturedRevision,
          capturedGeneration, candidateGeneration, candidateStamped,
          batchPhase, batchRevision, batchGeneration, publicationClaim,
          usedGenerations, badExactCommit, badGenerationReuse,
          badFailedRollback>>

TypeOK ==
    /\ rootRevision \in Revisions
    /\ rootGeneration \in GenerationValues
    /\ registryRevision \in RevisionValues
    /\ registryGeneration \in GenerationValues
    /\ registryStamped \in BOOLEAN
    /\ retired \in BOOLEAN
    /\ writerPhase \in [WriterIds -> WriterPhases]
    /\ writerObservedRevision \in [WriterIds -> RevisionValues]
    /\ writerObservedGeneration \in [WriterIds -> GenerationValues]
    /\ writerTargetRevision \in [WriterIds -> RevisionValues]
    /\ checkpointPhase \in CheckpointPhases
    /\ capturedRevision \in RevisionValues
    /\ capturedGeneration \in GenerationValues
    /\ candidateGeneration \in GenerationValues
    /\ candidateStamped \in BOOLEAN
    /\ batchPhase \in BatchPhases
    /\ batchRevision \in RevisionValues
    /\ batchGeneration \in GenerationValues
    /\ publicationClaim \in BOOLEAN
    /\ usedGenerations \subseteq Generations
    /\ badExactCommit \in BOOLEAN
    /\ badGenerationReuse \in BOOLEAN
    /\ badFailedRollback \in BOOLEAN

ExactRootRegistryAgreement ==
    rootGeneration = NoGeneration
    \/ /\ registryGeneration = rootGeneration
       /\ registryRevision = rootRevision
       /\ registryStamped
       /\ ~retired

NoInexactCommit == ~badExactCommit
NoRetainedGenerationABA == ~badGenerationReuse
FailedPublicationPreservesRegistry == ~badFailedRollback

RootPairEquals(revision, generation) ==
    rootRevision = revision /\ rootGeneration = generation

BatchMatchesExactRoot ==
    /\ batchPhase = "Selected"
    /\ batchRevision = rootRevision
    /\ batchGeneration = rootGeneration
    /\ registryRevision = rootRevision
    /\ registryGeneration = rootGeneration
    /\ registryStamped
    /\ rootGeneration \in Generations
    /\ ~retired

FreshGeneration(generation) ==
    /\ generation \in Generations
    /\ generation \notin usedGenerations
    /\ generation # candidateGeneration

Init ==
    /\ rootRevision = CHOOSE revision \in Revisions : TRUE
    /\ rootGeneration = NoGeneration
    /\ registryRevision = NoRevision
    /\ registryGeneration = NoGeneration
    /\ registryStamped = FALSE
    /\ retired = FALSE
    /\ writerPhase = [writer \in WriterIds |-> "Idle"]
    /\ writerObservedRevision = [writer \in WriterIds |-> NoRevision]
    /\ writerObservedGeneration = [writer \in WriterIds |-> NoGeneration]
    /\ writerTargetRevision = [writer \in WriterIds |-> NoRevision]
    /\ checkpointPhase = "Idle"
    /\ capturedRevision = NoRevision
    /\ capturedGeneration = NoGeneration
    /\ candidateGeneration = NoGeneration
    /\ candidateStamped = FALSE
    /\ batchPhase = "Idle"
    /\ batchRevision = NoRevision
    /\ batchGeneration = NoGeneration
    /\ publicationClaim = FALSE
    /\ usedGenerations = {}
    /\ badExactCommit = FALSE
    /\ badGenerationReuse = FALSE
    /\ badFailedRollback = FALSE

BeginWal(writer, target) ==
    /\ writer \in WriterIds
    /\ target \in Revisions \ {rootRevision}
    /\ writerPhase[writer] = "Idle"
    /\ writerPhase' = [writerPhase EXCEPT ![writer] = "WalDurable"]
    /\ writerObservedRevision' =
         [writerObservedRevision EXCEPT ![writer] = rootRevision]
    /\ writerObservedGeneration' =
         [writerObservedGeneration EXCEPT ![writer] = rootGeneration]
    /\ writerTargetRevision' =
         [writerTargetRevision EXCEPT ![writer] = target]
    /\ UNCHANGED <<rootRevision, rootGeneration, registryRevision,
                    registryGeneration, registryStamped, retired,
                    checkpointPhase, capturedRevision, capturedGeneration,
                    candidateGeneration, candidateStamped, batchPhase,
                    batchRevision, batchGeneration, publicationClaim,
                    usedGenerations, badExactCommit, badGenerationReuse,
                    badFailedRollback>>

RetrySemanticCas(writer) ==
    /\ writer \in WriterIds
    /\ writerPhase[writer] = "WalDurable"
    /\ ~RootPairEquals(writerObservedRevision[writer],
                       writerObservedGeneration[writer])
    /\ writerObservedRevision' =
         [writerObservedRevision EXCEPT ![writer] = rootRevision]
    /\ writerObservedGeneration' =
         [writerObservedGeneration EXCEPT ![writer] = rootGeneration]
    /\ UNCHANGED <<rootRevision, rootGeneration, registryRevision,
                    registryGeneration, registryStamped, retired, writerPhase,
                    writerTargetRevision, checkpointPhase, capturedRevision,
                    capturedGeneration, candidateGeneration, candidateStamped,
                    batchPhase, batchRevision, batchGeneration,
                    publicationClaim, usedGenerations, badExactCommit,
                    badGenerationReuse, badFailedRollback>>

SemanticVisibilityCas(writer) ==
    /\ writer \in WriterIds
    /\ writerPhase[writer] = "WalDurable"
    /\ RootPairEquals(writerObservedRevision[writer],
                      writerObservedGeneration[writer])
    /\ writerTargetRevision[writer] \in Revisions \ {rootRevision}
    /\ rootRevision' = writerTargetRevision[writer]
    /\ rootGeneration' =
         IF UnsafeSemanticPreservesBinding
         THEN rootGeneration
         ELSE NoGeneration
    /\ writerPhase' = [writerPhase EXCEPT ![writer] = "Idle"]
    /\ writerObservedRevision' =
         [writerObservedRevision EXCEPT ![writer] = NoRevision]
    /\ writerObservedGeneration' =
         [writerObservedGeneration EXCEPT ![writer] = NoGeneration]
    /\ writerTargetRevision' =
         [writerTargetRevision EXCEPT ![writer] = NoRevision]
    /\ UNCHANGED <<registryRevision, registryGeneration, registryStamped,
                    retired, checkpointPhase, capturedRevision,
                    capturedGeneration, candidateGeneration, candidateStamped,
                    batchPhase, batchRevision, batchGeneration,
                    publicationClaim, usedGenerations, badExactCommit,
                    badGenerationReuse, badFailedRollback>>

PrepareCheckpoint(generation) ==
    /\ checkpointPhase = "Idle"
    /\ ~publicationClaim
    /\ ~retired
    /\ IF UnsafeReuseRetainedGeneration
          THEN generation \in Generations
          ELSE FreshGeneration(generation)
    /\ checkpointPhase' = "Prepared"
    /\ capturedRevision' = rootRevision
    /\ capturedGeneration' = rootGeneration
    /\ candidateGeneration' = generation
    /\ candidateStamped' = FALSE
    /\ UNCHANGED <<rootRevision, rootGeneration, registryRevision,
                    registryGeneration, registryStamped, retired, writerPhase,
                    writerObservedRevision, writerObservedGeneration,
                    writerTargetRevision, batchPhase, batchRevision,
                    batchGeneration, publicationClaim, usedGenerations,
                    badExactCommit, badGenerationReuse, badFailedRollback>>

StampCheckpoint ==
    /\ checkpointPhase = "Prepared"
    /\ checkpointPhase' = "Stamped"
    /\ candidateStamped' = TRUE
    /\ UNCHANGED <<rootRevision, rootGeneration, registryRevision,
                    registryGeneration, registryStamped, retired, writerPhase,
                    writerObservedRevision, writerObservedGeneration,
                    writerTargetRevision, capturedRevision, capturedGeneration,
                    candidateGeneration, batchPhase, batchRevision,
                    batchGeneration, publicationClaim, usedGenerations,
                    badExactCommit, badGenerationReuse, badFailedRollback>>

ClaimPublication ==
    /\ checkpointPhase \in {"Prepared", "Stamped"}
    /\ ~publicationClaim
    /\ publicationClaim' = TRUE
    /\ UNCHANGED <<rootRevision, rootGeneration, registryRevision,
                    registryGeneration, registryStamped, retired, writerPhase,
                    writerObservedRevision, writerObservedGeneration,
                    writerTargetRevision, checkpointPhase, capturedRevision,
                    capturedGeneration, candidateGeneration, candidateStamped,
                    batchPhase, batchRevision, batchGeneration,
                    usedGenerations, badExactCommit, badGenerationReuse,
                    badFailedRollback>>

PublishCheckpoint ==
    /\ publicationClaim
    /\ checkpointPhase \in {"Prepared", "Stamped"}
    /\ (candidateStamped \/ UnsafePublishBeforeStamp)
    /\ RootPairEquals(capturedRevision, capturedGeneration)
    /\ ~retired
    /\ registryRevision' = capturedRevision
    /\ registryGeneration' = candidateGeneration
    /\ registryStamped' = candidateStamped
    /\ rootGeneration' = candidateGeneration
    /\ checkpointPhase' = "Idle"
    /\ capturedRevision' = NoRevision
    /\ capturedGeneration' = NoGeneration
    /\ candidateGeneration' = NoGeneration
    /\ candidateStamped' = FALSE
    /\ publicationClaim' = FALSE
    /\ usedGenerations' = usedGenerations \cup {candidateGeneration}
    /\ badGenerationReuse' =
         (badGenerationReuse \/ candidateGeneration \in usedGenerations)
    /\ UNCHANGED <<rootRevision, retired, writerPhase,
                    writerObservedRevision, writerObservedGeneration,
                    writerTargetRevision, batchPhase, batchRevision,
                    batchGeneration, badExactCommit, badFailedRollback>>

AbortCheckpoint ==
    /\ publicationClaim
    /\ checkpointPhase \in {"Prepared", "Stamped"}
    /\ (~RootPairEquals(capturedRevision, capturedGeneration) \/ retired)
    /\ registryRevision' =
         IF UnsafeFailedPublishKeepsRegistry
         THEN capturedRevision
         ELSE registryRevision
    /\ registryGeneration' =
         IF UnsafeFailedPublishKeepsRegistry
         THEN candidateGeneration
         ELSE registryGeneration
    /\ registryStamped' =
         IF UnsafeFailedPublishKeepsRegistry
         THEN candidateStamped
         ELSE registryStamped
    /\ badFailedRollback' =
         (badFailedRollback \/ UnsafeFailedPublishKeepsRegistry)
    /\ checkpointPhase' = "Idle"
    /\ capturedRevision' = NoRevision
    /\ capturedGeneration' = NoGeneration
    /\ candidateGeneration' = NoGeneration
    /\ candidateStamped' = FALSE
    /\ publicationClaim' = FALSE
    /\ UNCHANGED <<rootRevision, rootGeneration, retired, writerPhase,
                    writerObservedRevision, writerObservedGeneration,
                    writerTargetRevision, batchPhase, batchRevision,
                    batchGeneration, usedGenerations, badExactCommit,
                    badGenerationReuse>>

SelectExactBatch ==
    /\ batchPhase = "Idle"
    /\ ExactRootRegistryAgreement
    /\ rootGeneration \in Generations
    /\ batchPhase' = "Selected"
    /\ batchRevision' = rootRevision
    /\ batchGeneration' = rootGeneration
    /\ UNCHANGED <<rootRevision, rootGeneration, registryRevision,
                    registryGeneration, registryStamped, retired, writerPhase,
                    writerObservedRevision, writerObservedGeneration,
                    writerTargetRevision, checkpointPhase, capturedRevision,
                    capturedGeneration, candidateGeneration, candidateStamped,
                    publicationClaim, usedGenerations, badExactCommit,
                    badGenerationReuse, badFailedRollback>>

CommitExactBatch ==
    /\ batchPhase = "Selected"
    /\ (BatchMatchesExactRoot \/ UnsafeExactCommitIgnoresRoot)
    /\ badExactCommit' = (badExactCommit \/ ~BatchMatchesExactRoot)
    /\ batchPhase' = "Idle"
    /\ batchRevision' = NoRevision
    /\ batchGeneration' = NoGeneration
    /\ UNCHANGED <<rootRevision, rootGeneration, registryRevision,
                    registryGeneration, registryStamped, retired, writerPhase,
                    writerObservedRevision, writerObservedGeneration,
                    writerTargetRevision, checkpointPhase, capturedRevision,
                    capturedGeneration, candidateGeneration, candidateStamped,
                    publicationClaim, usedGenerations, badGenerationReuse,
                    badFailedRollback>>

DropExactBatch ==
    /\ batchPhase = "Selected"
    /\ batchPhase' = "Idle"
    /\ batchRevision' = NoRevision
    /\ batchGeneration' = NoGeneration
    /\ UNCHANGED <<rootRevision, rootGeneration, registryRevision,
                    registryGeneration, registryStamped, retired, writerPhase,
                    writerObservedRevision, writerObservedGeneration,
                    writerTargetRevision, checkpointPhase, capturedRevision,
                    capturedGeneration, candidateGeneration, candidateStamped,
                    publicationClaim, usedGenerations, badExactCommit,
                    badGenerationReuse, badFailedRollback>>

RetireCoordinator ==
    /\ ~retired
    /\ ~publicationClaim
    /\ retired' = TRUE
    /\ rootGeneration' =
         IF UnsafeRetireKeepsBinding THEN rootGeneration ELSE NoGeneration
    /\ UNCHANGED <<rootRevision, registryRevision, registryGeneration,
                    registryStamped, writerPhase, writerObservedRevision,
                    writerObservedGeneration, writerTargetRevision,
                    checkpointPhase, capturedRevision, capturedGeneration,
                    candidateGeneration, candidateStamped, batchPhase,
                    batchRevision, batchGeneration, publicationClaim,
                    usedGenerations, badExactCommit, badGenerationReuse,
                    badFailedRollback>>

Next ==
    \/ \E writer \in WriterIds, target \in Revisions : BeginWal(writer, target)
    \/ \E writer \in WriterIds : RetrySemanticCas(writer)
    \/ \E writer \in WriterIds : SemanticVisibilityCas(writer)
    \/ \E generation \in Generations : PrepareCheckpoint(generation)
    \/ StampCheckpoint
    \/ ClaimPublication
    \/ PublishCheckpoint
    \/ AbortCheckpoint
    \/ SelectExactBatch
    \/ CommitExactBatch
    \/ DropExactBatch
    \/ RetireCoordinator

Spec == Init /\ [][Next]_vars

=============================================================================
