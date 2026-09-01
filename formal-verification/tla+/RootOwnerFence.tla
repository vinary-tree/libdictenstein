--------------------------- MODULE RootOwnerFence ---------------------------
(*****************************************************************************)
(* Root-carried coordinator ownership without callback or lifecycle locks.   *)
(*                                                                           *)
(* A checkpoint captures the exact root revision and its owner. Retirement   *)
(* moves the owner to Retiring, publishes an unconditional fresh unbound     *)
(* root fence, and only then marks the owner Retired. A publisher paused      *)
(* before the fence may win first, in which case retirement fences that      *)
(* newer revision; after the fence, the publisher's exact CAS must lose.      *)
(*****************************************************************************)

EXTENDS Naturals, FiniteSets, TLC

CONSTANTS Revisions, Owners, UnsafePublishIgnoresExpectedRoot

ASSUME /\ Cardinality(Revisions) >= 3
       /\ Owners # {}

NoOwner == "NoOwner"
OwnerValues == Owners \cup {NoOwner}
OwnerPhases == {"Active", "Retiring", "Retired"}

VARIABLES
    rootRevision,
    rootOwner,
    usedRevisions,
    slotOwner,
    ownerPhase,
    prepared,
    preparedExpectedRevision,
    preparedOwner,
    retirementFenceDone,
    badRetiredPublication

vars ==
    <<rootRevision, rootOwner, usedRevisions, slotOwner, ownerPhase, prepared,
      preparedExpectedRevision, preparedOwner, retirementFenceDone,
      badRetiredPublication>>

TypeOK ==
    /\ rootRevision \in Revisions
    /\ rootOwner \in OwnerValues
    /\ usedRevisions \subseteq Revisions
    /\ rootRevision \in usedRevisions
    /\ slotOwner \in OwnerValues
    /\ ownerPhase \in [Owners -> OwnerPhases]
    /\ prepared \in BOOLEAN
    /\ preparedExpectedRevision \in Revisions
    /\ preparedOwner \in Owners
    /\ retirementFenceDone \in BOOLEAN
    /\ badRetiredPublication \in BOOLEAN

RootNeverNamesRetiredOwner ==
    rootOwner = NoOwner \/ ownerPhase[rootOwner] # "Retired"

NoRetiredPublication == ~badRetiredPublication

Init ==
    LET initialRevision == CHOOSE revision \in Revisions : TRUE
        initialOwner == CHOOSE owner \in Owners : TRUE
    IN  /\ rootRevision = initialRevision
        /\ rootOwner = initialOwner
        /\ usedRevisions = {initialRevision}
        /\ slotOwner = initialOwner
        /\ ownerPhase =
             [owner \in Owners |->
                IF owner = initialOwner THEN "Active" ELSE "Retired"]
        /\ prepared = FALSE
        /\ preparedExpectedRevision = initialRevision
        /\ preparedOwner = initialOwner
        /\ retirementFenceDone = FALSE
        /\ badRetiredPublication = FALSE

BeginCheckpoint ==
    /\ ~prepared
    /\ rootOwner \in Owners
    /\ rootOwner = slotOwner
    /\ ownerPhase[rootOwner] = "Active"
    /\ prepared' = TRUE
    /\ preparedExpectedRevision' = rootRevision
    /\ preparedOwner' = rootOwner
    /\ UNCHANGED <<rootRevision, rootOwner, usedRevisions, slotOwner,
                    ownerPhase, retirementFenceDone, badRetiredPublication>>

PublishCheckpoint(revision) ==
    /\ prepared
    /\ revision \in Revisions \ usedRevisions
    /\ (rootRevision = preparedExpectedRevision
        \/ UnsafePublishIgnoresExpectedRoot)
    /\ rootRevision' = revision
    /\ rootOwner' = preparedOwner
    /\ usedRevisions' = usedRevisions \cup {revision}
    /\ prepared' = FALSE
    /\ badRetiredPublication' =
         (badRetiredPublication \/ ownerPhase[preparedOwner] = "Retired")
    /\ UNCHANGED <<slotOwner, ownerPhase, preparedExpectedRevision,
                    preparedOwner, retirementFenceDone>>

BeginRetirement ==
    /\ slotOwner \in Owners
    /\ ownerPhase[slotOwner] = "Active"
    /\ ownerPhase' = [ownerPhase EXCEPT ![slotOwner] = "Retiring"]
    /\ retirementFenceDone' = FALSE
    /\ UNCHANGED <<rootRevision, rootOwner, usedRevisions, slotOwner, prepared,
                    preparedExpectedRevision, preparedOwner,
                    badRetiredPublication>>

PublishRetirementFence(revision) ==
    /\ slotOwner \in Owners
    /\ ownerPhase[slotOwner] = "Retiring"
    /\ revision \in Revisions \ usedRevisions
    /\ rootRevision' = revision
    /\ rootOwner' = NoOwner
    /\ usedRevisions' = usedRevisions \cup {revision}
    /\ retirementFenceDone' = TRUE
    /\ UNCHANGED <<slotOwner, ownerPhase, prepared,
                    preparedExpectedRevision, preparedOwner,
                    badRetiredPublication>>

CompleteRetirement ==
    /\ slotOwner \in Owners
    /\ ownerPhase[slotOwner] = "Retiring"
    /\ retirementFenceDone
    /\ ownerPhase' = [ownerPhase EXCEPT ![slotOwner] = "Retired"]
    /\ slotOwner' = NoOwner
    /\ UNCHANGED <<rootRevision, rootOwner, usedRevisions, prepared,
                    preparedExpectedRevision, preparedOwner,
                    retirementFenceDone, badRetiredPublication>>

Next ==
    \/ BeginCheckpoint
    \/ \E revision \in Revisions : PublishCheckpoint(revision)
    \/ BeginRetirement
    \/ \E revision \in Revisions : PublishRetirementFence(revision)
    \/ CompleteRetirement

Spec == Init /\ [][Next]_vars

=============================================================================
