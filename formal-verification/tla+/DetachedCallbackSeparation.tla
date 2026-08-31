---------------------- MODULE DetachedCallbackSeparation ----------------------
(***************************************************************************)
(* Capability separation between exact root-bound eviction and the legacy  *)
(* materialized callback API.  Exact generations can authorize a commit     *)
(* only when captured with the current root revision.  Detached catalogs    *)
(* are immutable advisory snapshots: callbacks may retain them across       *)
(* replacement, but can never preserve or manufacture exact root authority. *)
(***************************************************************************)

EXTENDS FiniteSets, TLC

CONSTANTS
    Revisions,
    ExactGenerations,
    DetachedGenerations,
    UnsafeLegacyReadsExact,
    UnsafeDetachedAuthorizes,
    UnsafeCheckpointPopulatesDetached,
    UnsafeSemanticPreservesBinding,
    UnsafeCatalogAuthorizes

ASSUME /\ Revisions # {}
       /\ ExactGenerations # {}
       /\ DetachedGenerations # {}
       /\ ExactGenerations \cap DetachedGenerations = {}

NoRevision == "NoRevision"
NoGeneration == "NoGeneration"

RevisionValues == Revisions \cup {NoRevision}
GenerationValues == ExactGenerations \cup DetachedGenerations \cup {NoGeneration}

VARIABLES
    rootRevision,
    rootBinding,
    exactCatalog,
    detachedCatalog,
    usedRevisions,
    usedExactGenerations,
    usedDetachedGenerations,
    callbackActive,
    callbackSnapshot,
    capturedRootRevision,
    capturedExactGeneration,
    lastAction,
    badDetachedAuthority,
    badCatalogAuthority

vars ==
    <<rootRevision, rootBinding, exactCatalog, detachedCatalog,
      usedRevisions, usedExactGenerations, usedDetachedGenerations,
      callbackActive, callbackSnapshot, capturedRootRevision,
      capturedExactGeneration, lastAction, badDetachedAuthority,
      badCatalogAuthority>>

FreshRevision(revision) ==
    revision \in Revisions /\ revision \notin usedRevisions

FreshExactGeneration(generation) ==
    generation \in ExactGenerations /\ generation \notin usedExactGenerations

FreshDetachedGeneration(generation) ==
    generation \in DetachedGenerations
    /\ generation \notin usedDetachedGenerations

TypeOK ==
    /\ rootRevision \in Revisions
    /\ rootBinding \in ExactGenerations \cup {NoGeneration}
    /\ exactCatalog \in ExactGenerations \cup {NoGeneration}
    /\ detachedCatalog \in GenerationValues
    /\ usedRevisions \subseteq Revisions
    /\ usedExactGenerations \subseteq ExactGenerations
    /\ usedDetachedGenerations \subseteq DetachedGenerations
    /\ callbackActive \in BOOLEAN
    /\ callbackSnapshot \in GenerationValues
    /\ capturedRootRevision \in RevisionValues
    /\ capturedExactGeneration \in ExactGenerations \cup {NoGeneration}
    /\ lastAction \in {"Init", "Semantic", "Checkpoint", "ExactCommit", "Other"}
    /\ badDetachedAuthority \in BOOLEAN
    /\ badCatalogAuthority \in BOOLEAN

DetachedCatalogContainsOnlyDetached ==
    detachedCatalog \in DetachedGenerations \cup {NoGeneration}

DetachedCallbackHasOnlyDetachedCapability ==
    ~callbackActive \/ callbackSnapshot \in DetachedGenerations

RetainedDetachedSnapshotRemainsOwned ==
    ~callbackActive \/ callbackSnapshot \in usedDetachedGenerations

SemanticClearsExactAuthority ==
    lastAction # "Semantic" \/ rootBinding = NoGeneration

DetachedNeverAuthorizesExactCommit == ~badDetachedAuthority

CatalogNeverAuthorizesExactCommit == ~badCatalogAuthority

Init ==
    LET initialRevision == CHOOSE revision \in Revisions : TRUE
        initialExact == CHOOSE generation \in ExactGenerations : TRUE
        initialDetached == CHOOSE generation \in DetachedGenerations : TRUE
    IN  /\ rootRevision = initialRevision
        /\ rootBinding = initialExact
        /\ exactCatalog = initialExact
        /\ detachedCatalog = initialDetached
        /\ usedRevisions = {initialRevision}
        /\ usedExactGenerations = {initialExact}
        /\ usedDetachedGenerations = {initialDetached}
        /\ callbackActive = FALSE
        /\ callbackSnapshot = NoGeneration
        /\ capturedRootRevision = NoRevision
        /\ capturedExactGeneration = NoGeneration
        /\ lastAction = "Init"
        /\ badDetachedAuthority = FALSE
        /\ badCatalogAuthority = FALSE

PublishSemantic(revision) ==
    /\ FreshRevision(revision)
    /\ rootRevision' = revision
    /\ rootBinding' =
         IF UnsafeSemanticPreservesBinding THEN rootBinding ELSE NoGeneration
    /\ usedRevisions' = usedRevisions \cup {revision}
    /\ lastAction' = "Semantic"
    /\ UNCHANGED <<exactCatalog, detachedCatalog, usedExactGenerations,
                    usedDetachedGenerations, callbackActive, callbackSnapshot,
                    capturedRootRevision, capturedExactGeneration,
                    badDetachedAuthority, badCatalogAuthority>>

PublishCheckpoint(revision, generation) ==
    /\ FreshRevision(revision)
    /\ FreshExactGeneration(generation)
    /\ rootRevision' = revision
    /\ rootBinding' = generation
    /\ exactCatalog' = generation
    /\ detachedCatalog' =
         IF UnsafeCheckpointPopulatesDetached THEN generation ELSE detachedCatalog
    /\ usedRevisions' = usedRevisions \cup {revision}
    /\ usedExactGenerations' = usedExactGenerations \cup {generation}
    /\ lastAction' = "Checkpoint"
    /\ UNCHANGED <<usedDetachedGenerations, callbackActive,
                    callbackSnapshot, capturedRootRevision,
                    capturedExactGeneration, badDetachedAuthority,
                    badCatalogAuthority>>

InstallDetached(generation) ==
    /\ FreshDetachedGeneration(generation)
    /\ detachedCatalog' = generation
    /\ usedDetachedGenerations' = usedDetachedGenerations \cup {generation}
    /\ lastAction' = "Other"
    /\ UNCHANGED <<rootRevision, rootBinding, exactCatalog, usedRevisions,
                    usedExactGenerations, callbackActive, callbackSnapshot,
                    capturedRootRevision, capturedExactGeneration,
                    badDetachedAuthority, badCatalogAuthority>>

ClearDetached ==
    /\ detachedCatalog' = NoGeneration
    /\ lastAction' = "Other"
    /\ UNCHANGED <<rootRevision, rootBinding, exactCatalog, usedRevisions,
                    usedExactGenerations, usedDetachedGenerations,
                    callbackActive, callbackSnapshot, capturedRootRevision,
                    capturedExactGeneration, badDetachedAuthority,
                    badCatalogAuthority>>

BeginDetachedCallback ==
    /\ ~callbackActive
    /\ detachedCatalog # NoGeneration
    /\ callbackActive' = TRUE
    /\ callbackSnapshot' =
         IF UnsafeLegacyReadsExact THEN exactCatalog ELSE detachedCatalog
    /\ lastAction' = "Other"
    /\ UNCHANGED <<rootRevision, rootBinding, exactCatalog, detachedCatalog,
                    usedRevisions, usedExactGenerations,
                    usedDetachedGenerations, capturedRootRevision,
                    capturedExactGeneration, badDetachedAuthority,
                    badCatalogAuthority>>

EndDetachedCallback ==
    /\ callbackActive
    /\ callbackActive' = FALSE
    /\ callbackSnapshot' = NoGeneration
    /\ lastAction' = "Other"
    /\ UNCHANGED <<rootRevision, rootBinding, exactCatalog, detachedCatalog,
                    usedRevisions, usedExactGenerations,
                    usedDetachedGenerations, capturedRootRevision,
                    capturedExactGeneration, badDetachedAuthority,
                    badCatalogAuthority>>

CaptureExactCommit ==
    /\ rootBinding \in ExactGenerations
    /\ capturedRootRevision' = rootRevision
    /\ capturedExactGeneration' = rootBinding
    /\ lastAction' = "Other"
    /\ UNCHANGED <<rootRevision, rootBinding, exactCatalog, detachedCatalog,
                    usedRevisions, usedExactGenerations,
                    usedDetachedGenerations, callbackActive,
                    callbackSnapshot, badDetachedAuthority,
                    badCatalogAuthority>>

PublishCapturedExactCommit(revision) ==
    /\ FreshRevision(revision)
    /\ capturedRootRevision = rootRevision
    /\ capturedExactGeneration = rootBinding
    /\ capturedExactGeneration \in ExactGenerations
    /\ rootRevision' = revision
    /\ usedRevisions' = usedRevisions \cup {revision}
    /\ capturedRootRevision' = NoRevision
    /\ capturedExactGeneration' = NoGeneration
    /\ lastAction' = "ExactCommit"
    /\ UNCHANGED <<rootBinding, exactCatalog, detachedCatalog,
                    usedExactGenerations, usedDetachedGenerations,
                    callbackActive, callbackSnapshot, badDetachedAuthority,
                    badCatalogAuthority>>

TryDetachedExactCommit(revision) ==
    /\ UnsafeDetachedAuthorizes
    /\ callbackActive
    /\ FreshRevision(revision)
    /\ rootRevision' = revision
    /\ usedRevisions' = usedRevisions \cup {revision}
    /\ badDetachedAuthority' = TRUE
    /\ lastAction' = "ExactCommit"
    /\ UNCHANGED <<rootBinding, exactCatalog, detachedCatalog,
                    usedExactGenerations, usedDetachedGenerations,
                    callbackActive, callbackSnapshot, capturedRootRevision,
                    capturedExactGeneration, badCatalogAuthority>>

TryCatalogExactCommit(revision) ==
    /\ UnsafeCatalogAuthorizes
    /\ exactCatalog \in ExactGenerations
    /\ exactCatalog # rootBinding
    /\ FreshRevision(revision)
    /\ rootRevision' = revision
    /\ usedRevisions' = usedRevisions \cup {revision}
    /\ badCatalogAuthority' = TRUE
    /\ lastAction' = "ExactCommit"
    /\ UNCHANGED <<rootBinding, exactCatalog, detachedCatalog,
                    usedExactGenerations, usedDetachedGenerations,
                    callbackActive, callbackSnapshot, capturedRootRevision,
                    capturedExactGeneration, badDetachedAuthority>>

Next ==
    \/ \E revision \in Revisions : PublishSemantic(revision)
    \/ \E revision \in Revisions, generation \in ExactGenerations :
         PublishCheckpoint(revision, generation)
    \/ \E generation \in DetachedGenerations : InstallDetached(generation)
    \/ ClearDetached
    \/ BeginDetachedCallback
    \/ EndDetachedCallback
    \/ CaptureExactCommit
    \/ \E revision \in Revisions : PublishCapturedExactCommit(revision)
    \/ \E revision \in Revisions : TryDetachedExactCommit(revision)
    \/ \E revision \in Revisions : TryCatalogExactCommit(revision)

Spec == Init /\ [][Next]_vars

=============================================================================
