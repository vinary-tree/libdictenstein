------------------------- MODULE HelpedRootResidency -------------------------
(*****************************************************************************)
(* Root-linearized residency with revision-fenced, helped materialization.    *)
(*                                                                           *)
(* The immutable root revision is the sole logical authority. A successful   *)
(* fault/eviction CAS publishes both the trie and its logical residency. The  *)
(* mutable word array is only a materialized cache of that root-carried       *)
(* revision. Every word CAS is fenced by the exact predecessor cell, which    *)
(* prevents a delayed helper for an old fault from resurrecting a bit after  *)
(* a newer re-eviction (and symmetrically prevents a stale clear).            *)
(*                                                                           *)
(* A release frontier advances only after every affected tagged word equals  *)
(* the descriptor target. Readers help, scan, and accept only after an exact  *)
(* root/generation/frontier revalidation. Generation identities are never    *)
(* reused while retained; rollover installs a fresh word array in the root    *)
(* CAS, so helpers retained from the previous generation cannot touch it.     *)
(*                                                                           *)
(* Coordinator retirement publishes an unbound fence revision before it      *)
(* clears the ArcSwap slot. A publisher that checked ACTIVE and then paused   *)
(* therefore loses its exact-root CAS after retirement. The discovery catalog*)
(* is deliberately non-authoritative.                                        *)
(*****************************************************************************)

EXTENDS Naturals, FiniteSets, TLC

CONSTANTS
    Revisions,
    Generations,
    Owners,
    Words,
    UnsafeUnfencedWordWrite,
    UnsafeEarlyFrontier,
    UnsafeRetirementWithoutFence,
    UnsafeCatalogAuthorizes,
    UnsafePublishWithoutStamp

ASSUME /\ Revisions # {}
       /\ Generations # {}
       /\ Owners # {}
       /\ Words # {}

NoRevision == "NoRevision"
NoGeneration == "NoGeneration"
NoOwner == "NoOwner"

RevisionValues == Revisions \cup {NoRevision}
GenerationValues == Generations \cup {NoGeneration}
OwnerValues == Owners \cup {NoOwner}
OwnerPhases == {"Active", "Retiring", "Retired"}
DescriptorPhases == {"Idle", "Published"}
DelayedPhases == {"Idle", "Captured"}

Cell == [bit : BOOLEAN, tag : Revisions]
Cells == [Words -> Cell]
Bits == [Words -> BOOLEAN]

VARIABLES
    rootRevision,
    rootGeneration,
    rootOwner,
    rootBits,
    liveBits,
    usedRevisions,
    usedGenerations,
    materializedGeneration,
    materializedCells,
    frontierRevision,
    descriptorPhase,
    descriptorGeneration,
    descriptorRevision,
    descriptorPredecessor,
    descriptorAffected,
    descriptorExpected,
    descriptorTarget,
    delayedPhase,
    delayedGeneration,
    delayedRevision,
    delayedWord,
    delayedExpected,
    delayedTarget,
    ownerPhase,
    slotOwner,
    retirementFenceDone,
    prepared,
    preparedExpectedRevision,
    preparedOwner,
    candidateGeneration,
    candidateStamped,
    catalogGeneration,
    badEarlyFrontier,
    badRetiredPublication,
    badCatalogCommit,
    badUnstampedPublication

vars ==
    <<rootRevision, rootGeneration, rootOwner, rootBits, liveBits,
      usedRevisions, usedGenerations, materializedGeneration,
      materializedCells, frontierRevision, descriptorPhase,
      descriptorGeneration, descriptorRevision, descriptorPredecessor,
      descriptorAffected, descriptorExpected, descriptorTarget, delayedPhase,
      delayedGeneration, delayedRevision, delayedWord, delayedExpected,
      delayedTarget, ownerPhase, slotOwner, retirementFenceDone, prepared,
      preparedExpectedRevision, preparedOwner, candidateGeneration,
      candidateStamped, catalogGeneration,
      badEarlyFrontier, badRetiredPublication, badCatalogCommit,
      badUnstampedPublication>>

BitsOf(cells) == [word \in Words |-> cells[word].bit]

FreshRevision(revision) ==
    revision \in Revisions /\ revision \notin usedRevisions

FreshGeneration(generation) ==
    generation \in Generations /\ generation \notin usedGenerations

DescriptorComplete ==
    \A word \in descriptorAffected :
        materializedCells[word] =
            [bit |-> descriptorTarget[word], tag |-> descriptorRevision]

TypeOK ==
    /\ rootRevision \in Revisions
    /\ rootGeneration \in GenerationValues
    /\ rootOwner \in OwnerValues
    /\ rootBits \in Bits
    /\ liveBits \in Bits
    /\ usedRevisions \subseteq Revisions
    /\ usedGenerations \subseteq Generations
    /\ rootRevision \in usedRevisions
    /\ materializedGeneration \in Generations
    /\ materializedCells \in Cells
    /\ frontierRevision \in Revisions
    /\ descriptorPhase \in DescriptorPhases
    /\ descriptorGeneration \in GenerationValues
    /\ descriptorRevision \in RevisionValues
    /\ descriptorPredecessor \in RevisionValues
    /\ descriptorAffected \subseteq Words
    /\ descriptorExpected \in Cells
    /\ descriptorTarget \in Bits
    /\ delayedPhase \in DelayedPhases
    /\ delayedGeneration \in GenerationValues
    /\ delayedRevision \in RevisionValues
    /\ delayedWord \in Words
    /\ delayedExpected \in Cell
    /\ delayedTarget \in BOOLEAN
    /\ ownerPhase \in [Owners -> OwnerPhases]
    /\ slotOwner \in OwnerValues
    /\ retirementFenceDone \in BOOLEAN
    /\ prepared \in BOOLEAN
    /\ preparedExpectedRevision \in RevisionValues
    /\ preparedOwner \in OwnerValues
    /\ candidateGeneration \in GenerationValues
    /\ candidateStamped \in BOOLEAN
    /\ catalogGeneration \in GenerationValues
    /\ badEarlyFrontier \in BOOLEAN
    /\ badRetiredPublication \in BOOLEAN
    /\ badCatalogCommit \in BOOLEAN
    /\ badUnstampedPublication \in BOOLEAN

RootIsSoleLogicalAuthority ==
    rootGeneration = NoGeneration
    \/ /\ rootOwner \in Owners
       /\ ownerPhase[rootOwner] # "Retired"
       /\ rootBits = liveBits

MaterializedResidencyMatchesPublishedRoot ==
    (rootGeneration = materializedGeneration
     /\ rootRevision = frontierRevision)
    => BitsOf(materializedCells) = rootBits

PublishedDescriptorMatchesRoot ==
    descriptorPhase = "Published" =>
        /\ descriptorGeneration = materializedGeneration
        /\ descriptorRevision \in usedRevisions
        /\ descriptorPredecessor = frontierRevision

NoEarlyFrontier == ~badEarlyFrontier
NoRetiredPublication == ~badRetiredPublication
CatalogNeverAuthorizes == ~badCatalogCommit
NoUnstampedPublication == ~badUnstampedPublication

Init ==
    LET initialRevision == CHOOSE revision \in Revisions : TRUE
        initialGeneration == CHOOSE generation \in Generations : TRUE
        initialOwner == CHOOSE owner \in Owners : TRUE
        initialBits == [word \in Words |-> FALSE]
    IN  /\ rootRevision = initialRevision
        /\ rootGeneration = initialGeneration
        /\ rootOwner = initialOwner
        /\ rootBits = initialBits
        /\ liveBits = initialBits
        /\ usedRevisions = {initialRevision}
        /\ usedGenerations = {initialGeneration}
        /\ materializedGeneration = initialGeneration
        /\ materializedCells =
             [word \in Words |-> [bit |-> FALSE, tag |-> initialRevision]]
        /\ frontierRevision = initialRevision
        /\ descriptorPhase = "Idle"
        /\ descriptorGeneration = NoGeneration
        /\ descriptorRevision = NoRevision
        /\ descriptorPredecessor = NoRevision
        /\ descriptorAffected = {}
        /\ descriptorExpected = materializedCells
        /\ descriptorTarget = initialBits
        /\ delayedPhase = "Idle"
        /\ delayedGeneration = NoGeneration
        /\ delayedRevision = NoRevision
        /\ delayedWord = CHOOSE word \in Words : TRUE
        /\ delayedExpected = [bit |-> FALSE, tag |-> initialRevision]
        /\ delayedTarget = FALSE
        /\ ownerPhase =
             [owner \in Owners |->
                IF owner = initialOwner THEN "Active" ELSE "Retired"]
        /\ slotOwner = initialOwner
        /\ retirementFenceDone = FALSE
        /\ prepared = FALSE
        /\ preparedExpectedRevision = NoRevision
        /\ preparedOwner = NoOwner
        /\ candidateGeneration = NoGeneration
        /\ candidateStamped = FALSE
        /\ catalogGeneration = initialGeneration
        /\ badEarlyFrontier = FALSE
        /\ badRetiredPublication = FALSE
        /\ badCatalogCommit = FALSE
        /\ badUnstampedPublication = FALSE

(***************************************************************************)
(* Exact fault/eviction root CAS. All descriptor storage is prepared before *)
(* this action. The action changes logical root/tree residency atomically and*)
(* does not touch a materialized word before it wins.                       *)
(***************************************************************************)
PublishRootTransition(word, revision) ==
    /\ word \in Words
    /\ FreshRevision(revision)
    /\ rootGeneration \in Generations
    /\ rootGeneration = materializedGeneration
    /\ rootOwner \in Owners
    /\ ownerPhase[rootOwner] = "Active"
    /\ descriptorPhase = "Idle"
    /\ frontierRevision = rootRevision
    /\ rootRevision' = revision
    /\ rootBits' = [rootBits EXCEPT ![word] = ~@]
    /\ liveBits' = [liveBits EXCEPT ![word] = ~@]
    /\ usedRevisions' = usedRevisions \cup {revision}
    /\ descriptorPhase' = "Published"
    /\ descriptorGeneration' = rootGeneration
    /\ descriptorRevision' = revision
    /\ descriptorPredecessor' = rootRevision
    /\ descriptorAffected' = {word}
    /\ descriptorExpected' = materializedCells
    /\ descriptorTarget' = [rootBits EXCEPT ![word] = ~@]
    /\ UNCHANGED <<rootGeneration, rootOwner, usedGenerations,
                    materializedGeneration, materializedCells,
                    frontierRevision, delayedPhase, delayedGeneration,
                    delayedRevision, delayedWord, delayedExpected,
                    delayedTarget, ownerPhase, slotOwner,
                    retirementFenceDone, prepared, preparedExpectedRevision,
                    preparedOwner, candidateGeneration, candidateStamped,
                    catalogGeneration,
                    badEarlyFrontier, badRetiredPublication,
                    badCatalogCommit, badUnstampedPublication>>

CaptureDelayedHelper(word) ==
    /\ descriptorPhase = "Published"
    /\ delayedPhase = "Idle"
    /\ word \in descriptorAffected
    /\ delayedPhase' = "Captured"
    /\ delayedGeneration' = descriptorGeneration
    /\ delayedRevision' = descriptorRevision
    /\ delayedWord' = word
    /\ delayedExpected' = descriptorExpected[word]
    /\ delayedTarget' = descriptorTarget[word]
    /\ UNCHANGED <<rootRevision, rootGeneration, rootOwner, rootBits,
                    liveBits, usedRevisions, usedGenerations,
                    materializedGeneration, materializedCells,
                    frontierRevision, descriptorPhase, descriptorGeneration,
                    descriptorRevision, descriptorPredecessor,
                    descriptorAffected, descriptorExpected, descriptorTarget,
                    ownerPhase, slotOwner, retirementFenceDone, prepared,
                    preparedExpectedRevision, preparedOwner,
                    candidateGeneration, candidateStamped, catalogGeneration,
                    badEarlyFrontier,
                    badRetiredPublication, badCatalogCommit,
                    badUnstampedPublication>>

(***************************************************************************)
(* Exact tagged predecessor CAS. Duplicate helpers are harmless: one wins;  *)
(* every later duplicate observes a different tag and cannot overwrite a    *)
(* successor, including an inverse transition.                              *)
(***************************************************************************)
HelpDescriptorWord(word) ==
    /\ descriptorPhase = "Published"
    /\ word \in descriptorAffected
    /\ descriptorGeneration = materializedGeneration
    /\ materializedCells[word] = descriptorExpected[word]
    /\ materializedCells' =
         [materializedCells EXCEPT
            ![word] = [bit |-> descriptorTarget[word],
                       tag |-> descriptorRevision]]
    /\ UNCHANGED <<rootRevision, rootGeneration, rootOwner, rootBits,
                    liveBits, usedRevisions, usedGenerations,
                    materializedGeneration, frontierRevision, descriptorPhase,
                    descriptorGeneration, descriptorRevision,
                    descriptorPredecessor, descriptorAffected,
                    descriptorExpected, descriptorTarget, delayedPhase,
                    delayedGeneration, delayedRevision, delayedWord,
                    delayedExpected, delayedTarget, ownerPhase, slotOwner,
                    retirementFenceDone, prepared, preparedExpectedRevision,
                    preparedOwner, candidateGeneration, candidateStamped,
                    catalogGeneration,
                    badEarlyFrontier, badRetiredPublication,
                    badCatalogCommit, badUnstampedPublication>>

AdvanceFrontier ==
    /\ descriptorPhase = "Published"
    /\ (UnsafeEarlyFrontier \/ DescriptorComplete)
    /\ badEarlyFrontier' =
         (badEarlyFrontier \/ ~DescriptorComplete)
    /\ frontierRevision' = descriptorRevision
    /\ descriptorPhase' = "Idle"
    /\ descriptorGeneration' = NoGeneration
    /\ descriptorRevision' = NoRevision
    /\ descriptorPredecessor' = NoRevision
    /\ descriptorAffected' = {}
    /\ UNCHANGED <<rootRevision, rootGeneration, rootOwner, rootBits,
                    liveBits, usedRevisions, usedGenerations,
                    materializedGeneration, materializedCells,
                    descriptorExpected, descriptorTarget, delayedPhase,
                    delayedGeneration, delayedRevision, delayedWord,
                    delayedExpected, delayedTarget, ownerPhase, slotOwner,
                    retirementFenceDone, prepared, preparedExpectedRevision,
                    preparedOwner, candidateGeneration, candidateStamped,
                    catalogGeneration,
                    badRetiredPublication, badCatalogCommit,
                    badUnstampedPublication>>

RunDelayedHelper ==
    /\ delayedPhase = "Captured"
    /\ materializedCells' =
         IF UnsafeUnfencedWordWrite
         THEN [materializedCells EXCEPT
                 ![delayedWord] = [bit |-> delayedTarget,
                                   tag |-> delayedRevision]]
         ELSE IF /\ delayedGeneration = materializedGeneration
                 /\ materializedCells[delayedWord] = delayedExpected
              THEN [materializedCells EXCEPT
                      ![delayedWord] = [bit |-> delayedTarget,
                                        tag |-> delayedRevision]]
              ELSE materializedCells
    /\ delayedPhase' = "Idle"
    /\ delayedGeneration' = NoGeneration
    /\ delayedRevision' = NoRevision
    /\ UNCHANGED <<rootRevision, rootGeneration, rootOwner, rootBits,
                    liveBits, usedRevisions, usedGenerations,
                    materializedGeneration, frontierRevision, descriptorPhase,
                    descriptorGeneration, descriptorRevision,
                    descriptorPredecessor, descriptorAffected,
                    descriptorExpected, descriptorTarget, delayedWord,
                    delayedExpected, delayedTarget, ownerPhase, slotOwner,
                    retirementFenceDone, prepared, preparedExpectedRevision,
                    preparedOwner, candidateGeneration, candidateStamped,
                    catalogGeneration,
                    badEarlyFrontier, badRetiredPublication,
                    badCatalogCommit, badUnstampedPublication>>

(***************************************************************************)
(* Semantic writers perform only their existing root CAS. They clear exact   *)
(* eviction authority and never load, increment, invalidate, or lock the     *)
(* registry/materialization path.                                           *)
(***************************************************************************)
PublishSemantic(revision) ==
    /\ FreshRevision(revision)
    /\ rootRevision' = revision
    /\ rootGeneration' = NoGeneration
    /\ rootOwner' = NoOwner
    /\ rootBits' = [word \in Words |-> FALSE]
    /\ liveBits' = [word \in Words |-> FALSE]
    /\ usedRevisions' = usedRevisions \cup {revision}
    /\ UNCHANGED <<usedGenerations, materializedGeneration,
                    materializedCells, frontierRevision, descriptorPhase,
                    descriptorGeneration, descriptorRevision,
                    descriptorPredecessor, descriptorAffected,
                    descriptorExpected, descriptorTarget, delayedPhase,
                    delayedGeneration, delayedRevision, delayedWord,
                    delayedExpected, delayedTarget, ownerPhase, slotOwner,
                    retirementFenceDone, prepared, preparedExpectedRevision,
                    preparedOwner, candidateGeneration, candidateStamped,
                    catalogGeneration,
                    badEarlyFrontier, badRetiredPublication,
                    badCatalogCommit, badUnstampedPublication>>

BeginCheckpoint(owner, generation) ==
    /\ owner \in Owners
    /\ generation \in Generations
    /\ slotOwner = owner
    /\ ownerPhase[owner] = "Active"
    /\ ~prepared
    /\ prepared' = TRUE
    /\ preparedExpectedRevision' = rootRevision
    /\ preparedOwner' = owner
    /\ candidateGeneration' = generation
    /\ candidateStamped' = ~UnsafePublishWithoutStamp
    /\ UNCHANGED <<rootRevision, rootGeneration, rootOwner, rootBits,
                    liveBits, usedRevisions, usedGenerations,
                    materializedGeneration, materializedCells,
                    frontierRevision, descriptorPhase, descriptorGeneration,
                    descriptorRevision, descriptorPredecessor,
                    descriptorAffected, descriptorExpected, descriptorTarget,
                    delayedPhase, delayedGeneration, delayedRevision,
                    delayedWord, delayedExpected, delayedTarget, ownerPhase,
                    slotOwner, retirementFenceDone,
                    catalogGeneration, badEarlyFrontier,
                    badRetiredPublication, badCatalogCommit,
                    badUnstampedPublication>>

PublishCheckpoint(revision) ==
    /\ prepared
    /\ FreshRevision(revision)
    /\ FreshGeneration(candidateGeneration)
    /\ rootRevision = preparedExpectedRevision
    /\ (candidateStamped \/ UnsafePublishWithoutStamp)
    /\ rootRevision' = revision
    /\ rootGeneration' = candidateGeneration
    /\ rootOwner' = preparedOwner
    /\ usedRevisions' = usedRevisions \cup {revision}
    /\ usedGenerations' = usedGenerations \cup {candidateGeneration}
    /\ materializedGeneration' = candidateGeneration
    /\ materializedCells' =
         [word \in Words |-> [bit |-> rootBits[word], tag |-> revision]]
    /\ frontierRevision' = revision
    /\ descriptorPhase' = "Idle"
    /\ descriptorGeneration' = NoGeneration
    /\ descriptorRevision' = NoRevision
    /\ descriptorPredecessor' = NoRevision
    /\ descriptorAffected' = {}
    /\ prepared' = FALSE
    /\ preparedExpectedRevision' = NoRevision
    /\ candidateStamped' = FALSE
    /\ catalogGeneration' = candidateGeneration
    /\ badRetiredPublication' =
         (badRetiredPublication \/ ownerPhase[preparedOwner] = "Retired")
    /\ badUnstampedPublication' =
         (badUnstampedPublication \/ ~candidateStamped)
    /\ UNCHANGED <<rootBits, liveBits, descriptorExpected,
                    descriptorTarget, delayedPhase, delayedGeneration,
                    delayedRevision, delayedWord, delayedExpected,
                    delayedTarget, ownerPhase, slotOwner,
                    retirementFenceDone, preparedOwner,
                    candidateGeneration, badEarlyFrontier, badCatalogCommit>>

BeginRetirement(owner) ==
    /\ owner \in Owners
    /\ slotOwner = owner
    /\ ownerPhase[owner] = "Active"
    /\ ownerPhase' = [ownerPhase EXCEPT ![owner] = "Retiring"]
    /\ retirementFenceDone' = FALSE
    /\ UNCHANGED <<rootRevision, rootGeneration, rootOwner, rootBits,
                    liveBits, usedRevisions, usedGenerations,
                    materializedGeneration, materializedCells,
                    frontierRevision, descriptorPhase, descriptorGeneration,
                    descriptorRevision, descriptorPredecessor,
                    descriptorAffected, descriptorExpected, descriptorTarget,
                    delayedPhase, delayedGeneration, delayedRevision,
                    delayedWord, delayedExpected, delayedTarget, slotOwner,
                    prepared, preparedExpectedRevision, preparedOwner,
                    candidateGeneration, candidateStamped, catalogGeneration,
                    badEarlyFrontier,
                    badRetiredPublication, badCatalogCommit,
                    badUnstampedPublication>>

PublishRetirementFence(revision) ==
    /\ slotOwner \in Owners
    /\ ownerPhase[slotOwner] = "Retiring"
    /\ FreshRevision(revision)
    /\ rootRevision' = revision
    /\ rootGeneration' = NoGeneration
    /\ rootOwner' = NoOwner
    /\ rootBits' = [word \in Words |-> FALSE]
    /\ liveBits' = [word \in Words |-> FALSE]
    /\ usedRevisions' = usedRevisions \cup {revision}
    /\ retirementFenceDone' = TRUE
    /\ UNCHANGED <<usedGenerations, materializedGeneration,
                    materializedCells, frontierRevision, descriptorPhase,
                    descriptorGeneration, descriptorRevision,
                    descriptorPredecessor, descriptorAffected,
                    descriptorExpected, descriptorTarget, delayedPhase,
                    delayedGeneration, delayedRevision, delayedWord,
                    delayedExpected, delayedTarget, ownerPhase, slotOwner,
                    prepared, preparedExpectedRevision, preparedOwner,
                    candidateGeneration, candidateStamped, catalogGeneration,
                    badEarlyFrontier,
                    badRetiredPublication, badCatalogCommit,
                    badUnstampedPublication>>

ClearRetiredSlot ==
    /\ slotOwner \in Owners
    /\ ownerPhase[slotOwner] = "Retiring"
    /\ (retirementFenceDone \/ UnsafeRetirementWithoutFence)
    /\ ownerPhase' = [ownerPhase EXCEPT ![slotOwner] = "Retired"]
    /\ slotOwner' = NoOwner
    /\ UNCHANGED <<rootRevision, rootGeneration, rootOwner, rootBits,
                    liveBits, usedRevisions, usedGenerations,
                    materializedGeneration, materializedCells,
                    frontierRevision, descriptorPhase, descriptorGeneration,
                    descriptorRevision, descriptorPredecessor,
                    descriptorAffected, descriptorExpected, descriptorTarget,
                    delayedPhase, delayedGeneration, delayedRevision,
                    delayedWord, delayedExpected, delayedTarget,
                    retirementFenceDone, prepared, preparedExpectedRevision,
                    preparedOwner, candidateGeneration, candidateStamped,
                    catalogGeneration,
                    badEarlyFrontier, badRetiredPublication,
                    badCatalogCommit, badUnstampedPublication>>

TryCatalogCommit ==
    /\ catalogGeneration \in Generations
    /\ catalogGeneration # rootGeneration
    /\ UnsafeCatalogAuthorizes
    /\ badCatalogCommit' = TRUE
    /\ UNCHANGED <<rootRevision, rootGeneration, rootOwner, rootBits,
                    liveBits, usedRevisions, usedGenerations,
                    materializedGeneration, materializedCells,
                    frontierRevision, descriptorPhase, descriptorGeneration,
                    descriptorRevision, descriptorPredecessor,
                    descriptorAffected, descriptorExpected, descriptorTarget,
                    delayedPhase, delayedGeneration, delayedRevision,
                    delayedWord, delayedExpected, delayedTarget, ownerPhase,
                    slotOwner, retirementFenceDone, prepared,
                    preparedExpectedRevision, preparedOwner,
                    candidateGeneration, candidateStamped, catalogGeneration,
                    badEarlyFrontier,
                    badRetiredPublication, badUnstampedPublication>>

Next ==
    \/ \E word \in Words, revision \in Revisions :
         PublishRootTransition(word, revision)
    \/ \E word \in Words : CaptureDelayedHelper(word)
    \/ \E word \in Words : HelpDescriptorWord(word)
    \/ AdvanceFrontier
    \/ RunDelayedHelper
    \/ \E revision \in Revisions : PublishSemantic(revision)
    \/ \E owner \in Owners, generation \in Generations :
         BeginCheckpoint(owner, generation)
    \/ \E revision \in Revisions : PublishCheckpoint(revision)
    \/ \E owner \in Owners : BeginRetirement(owner)
    \/ \E revision \in Revisions : PublishRetirementFence(revision)
    \/ ClearRetiredSlot
    \/ TryCatalogCommit

Spec == Init /\ [][Next]_vars

=============================================================================
