----------------------- MODULE HelpedCheckpointStamps -----------------------
(*****************************************************************************)
(* Root-published, idempotently helped durable stamps.                       *)
(*                                                                           *)
(* A checkpoint candidate is fully allocated and its backing storage is     *)
(* durable before the root CAS. A losing candidate is never stamped. A      *)
(* winning root publishes a pending catalog; the publisher or any observer  *)
(* may perform the idempotent stamp stores and activates the catalog only    *)
(* after all stores are visible. A semantic successor may detach authority  *)
(* while a helper is paused, but a successfully published durable backing   *)
(* remains valid, so the late store is harmless and cannot authorize exact  *)
(* eviction without the root binding.                                       *)
(*****************************************************************************)

EXTENDS Naturals, FiniteSets, TLC

CONSTANTS
    Revisions,
    Catalogs,
    Stamps,
    UnsafeActivateEarly,
    UnsafeStampFailedCandidate,
    UnsafeReclaimRetained

ASSUME /\ Cardinality(Revisions) >= 2
       /\ Catalogs # {}
       /\ Stamps # {}

NoCatalog == "NoCatalog"
CatalogValues == Catalogs \cup {NoCatalog}

VARIABLES
    rootRevision,
    rootCatalog,
    usedRevisions,
    prepared,
    candidateCatalog,
    candidateExpectedRevision,
    storageValid,
    everPublished,
    stampApplied,
    catalogActive,
    retainedHelpers,
    badEarlyActivation,
    badStampBeforePublication,
    badStampToInvalidStorage

vars ==
    <<rootRevision, rootCatalog, usedRevisions, prepared, candidateCatalog,
      candidateExpectedRevision, storageValid, everPublished, stampApplied,
      catalogActive, retainedHelpers, badEarlyActivation,
      badStampBeforePublication, badStampToInvalidStorage>>

AllStampsApplied(catalog) ==
    \A stamp \in Stamps : stampApplied[catalog][stamp]

TypeOK ==
    /\ rootRevision \in Revisions
    /\ rootCatalog \in CatalogValues
    /\ usedRevisions \subseteq Revisions
    /\ rootRevision \in usedRevisions
    /\ prepared \in BOOLEAN
    /\ candidateCatalog \in CatalogValues
    /\ candidateExpectedRevision \in Revisions
    /\ storageValid \in [Catalogs -> BOOLEAN]
    /\ everPublished \subseteq Catalogs
    /\ stampApplied \in [Catalogs -> [Stamps -> BOOLEAN]]
    /\ catalogActive \in [Catalogs -> BOOLEAN]
    /\ retainedHelpers \subseteq Catalogs
    /\ badEarlyActivation \in BOOLEAN
    /\ badStampBeforePublication \in BOOLEAN
    /\ badStampToInvalidStorage \in BOOLEAN

ActiveCatalogIsComplete ==
    \A catalog \in Catalogs :
      catalogActive[catalog] => AllStampsApplied(catalog)

PublishedStorageRemainsValid ==
    \A catalog \in everPublished : storageValid[catalog]

RootActiveCatalogIsUsable ==
    rootCatalog = NoCatalog
    \/ /\ rootCatalog \in Catalogs
       /\ (~catalogActive[rootCatalog] \/ AllStampsApplied(rootCatalog))
       /\ storageValid[rootCatalog]

NoEarlyActivation == ~badEarlyActivation
NoStampBeforePublication == ~badStampBeforePublication
NoStampToInvalidStorage == ~badStampToInvalidStorage

Init ==
    LET initialRevision == CHOOSE revision \in Revisions : TRUE
    IN  /\ rootRevision = initialRevision
        /\ rootCatalog = NoCatalog
        /\ usedRevisions = {initialRevision}
        /\ prepared = FALSE
        /\ candidateCatalog = NoCatalog
        /\ candidateExpectedRevision = initialRevision
        /\ storageValid = [catalog \in Catalogs |-> FALSE]
        /\ everPublished = {}
        /\ stampApplied =
             [catalog \in Catalogs |-> [stamp \in Stamps |-> FALSE]]
        /\ catalogActive = [catalog \in Catalogs |-> FALSE]
        /\ retainedHelpers = {}
        /\ badEarlyActivation = FALSE
        /\ badStampBeforePublication = FALSE
        /\ badStampToInvalidStorage = FALSE

BeginCheckpoint(catalog) ==
    /\ ~prepared
    /\ catalog \in Catalogs \ everPublished
    /\ ~storageValid[catalog]
    /\ prepared' = TRUE
    /\ candidateCatalog' = catalog
    /\ candidateExpectedRevision' = rootRevision
    /\ storageValid' = [storageValid EXCEPT ![catalog] = TRUE]
    /\ stampApplied' =
         [stampApplied EXCEPT ![catalog] = [stamp \in Stamps |-> FALSE]]
    /\ catalogActive' = [catalogActive EXCEPT ![catalog] = FALSE]
    /\ UNCHANGED <<rootRevision, rootCatalog, usedRevisions, everPublished,
                    retainedHelpers, badEarlyActivation,
                    badStampBeforePublication, badStampToInvalidStorage>>

PublishCheckpoint(revision) ==
    /\ prepared
    /\ revision \in Revisions \ usedRevisions
    /\ rootRevision = candidateExpectedRevision
    /\ rootRevision' = revision
    /\ rootCatalog' = candidateCatalog
    /\ usedRevisions' = usedRevisions \cup {revision}
    /\ everPublished' = everPublished \cup {candidateCatalog}
    /\ retainedHelpers' = retainedHelpers \cup {candidateCatalog}
    /\ prepared' = FALSE
    /\ candidateCatalog' = NoCatalog
    /\ UNCHANGED <<candidateExpectedRevision, storageValid, stampApplied,
                    catalogActive, badEarlyActivation,
                    badStampBeforePublication, badStampToInvalidStorage>>

PublishSemantic(revision) ==
    /\ revision \in Revisions \ usedRevisions
    /\ rootRevision' = revision
    /\ rootCatalog' = NoCatalog
    /\ usedRevisions' = usedRevisions \cup {revision}
    /\ UNCHANGED <<prepared, candidateCatalog, candidateExpectedRevision,
                    storageValid, everPublished, stampApplied, catalogActive,
                    retainedHelpers, badEarlyActivation,
                    badStampBeforePublication, badStampToInvalidStorage>>

AbortLosingCheckpoint ==
    /\ prepared
    /\ rootRevision # candidateExpectedRevision
    /\ storageValid' = [storageValid EXCEPT ![candidateCatalog] = FALSE]
    /\ prepared' = FALSE
    /\ candidateCatalog' = NoCatalog
    /\ UNCHANGED <<rootRevision, rootCatalog, usedRevisions,
                    candidateExpectedRevision, everPublished, stampApplied,
                    catalogActive, retainedHelpers, badEarlyActivation,
                    badStampBeforePublication, badStampToInvalidStorage>>

HelpStamp(catalog, stamp) ==
    /\ catalog \in Catalogs
    /\ stamp \in Stamps
    /\ catalog \in retainedHelpers
       \/ /\ UnsafeStampFailedCandidate
          /\ prepared
          /\ catalog = candidateCatalog
    /\ stampApplied' =
         [stampApplied EXCEPT ![catalog][stamp] = TRUE]
    /\ badStampBeforePublication' =
         (badStampBeforePublication \/ catalog \notin everPublished)
    /\ badStampToInvalidStorage' =
         (badStampToInvalidStorage \/ ~storageValid[catalog])
    /\ UNCHANGED <<rootRevision, rootCatalog, usedRevisions, prepared,
                    candidateCatalog, candidateExpectedRevision, storageValid,
                    everPublished, catalogActive, retainedHelpers,
                    badEarlyActivation>>

ActivateCatalog(catalog) ==
    /\ catalog \in retainedHelpers
    /\ (AllStampsApplied(catalog) \/ UnsafeActivateEarly)
    /\ catalogActive' = [catalogActive EXCEPT ![catalog] = TRUE]
    /\ badEarlyActivation' =
         (badEarlyActivation \/ ~AllStampsApplied(catalog))
    /\ UNCHANGED <<rootRevision, rootCatalog, usedRevisions, prepared,
                    candidateCatalog, candidateExpectedRevision, storageValid,
                    everPublished, stampApplied, retainedHelpers,
                    badStampBeforePublication, badStampToInvalidStorage>>

ReleaseHelper(catalog) ==
    /\ catalog \in retainedHelpers
    /\ catalogActive[catalog] \/ rootCatalog # catalog
    /\ retainedHelpers' = retainedHelpers \ {catalog}
    /\ UNCHANGED <<rootRevision, rootCatalog, usedRevisions, prepared,
                    candidateCatalog, candidateExpectedRevision, storageValid,
                    everPublished, stampApplied, catalogActive,
                    badEarlyActivation, badStampBeforePublication,
                    badStampToInvalidStorage>>

Reclaim(catalog) ==
    /\ catalog \in Catalogs
    /\ rootCatalog # catalog
    /\ ~prepared \/ candidateCatalog # catalog
    /\ catalog \notin everPublished
    /\ (catalog \notin retainedHelpers \/ UnsafeReclaimRetained)
    /\ storageValid' = [storageValid EXCEPT ![catalog] = FALSE]
    /\ UNCHANGED <<rootRevision, rootCatalog, usedRevisions, prepared,
                    candidateCatalog, candidateExpectedRevision,
                    everPublished, stampApplied, catalogActive,
                    retainedHelpers, badEarlyActivation,
                    badStampBeforePublication, badStampToInvalidStorage>>

Next ==
    \/ \E catalog \in Catalogs : BeginCheckpoint(catalog)
    \/ \E revision \in Revisions : PublishCheckpoint(revision)
    \/ \E revision \in Revisions : PublishSemantic(revision)
    \/ AbortLosingCheckpoint
    \/ \E catalog \in Catalogs, stamp \in Stamps : HelpStamp(catalog, stamp)
    \/ \E catalog \in Catalogs : ActivateCatalog(catalog)
    \/ \E catalog \in Catalogs : ReleaseHelper(catalog)
    \/ \E catalog \in Catalogs : Reclaim(catalog)

Spec == Init /\ [][Next]_vars

=============================================================================
