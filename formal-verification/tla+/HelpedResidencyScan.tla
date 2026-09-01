------------------------- MODULE HelpedResidencyScan -------------------------
(*****************************************************************************)
(* Multiword helped materialization and exact scan acceptance.               *)
(*                                                                           *)
(* A root CAS atomically publishes the logical target for an arbitrary       *)
(* nonempty set of residency words. Tagged word CASes may materialize that   *)
(* descriptor in any order. The frontier remains at the predecessor until    *)
(* every affected word is exact. A scan captures root identity and logical   *)
(* bits, may overlap partial helping, then accepts only after both root and   *)
(* frontier still identify the capture.                                      *)
(*****************************************************************************)

EXTENDS Naturals, FiniteSets, TLC

CONSTANTS Revisions, Words, UnsafeScanSkipsRevalidation, UnsafeEarlyFrontier

ASSUME /\ Revisions # {}
       /\ Words # {}

NoRevision == "NoRevision"
Cell == [bit : BOOLEAN, tag : Revisions]
Cells == [Words -> Cell]
Bits == [Words -> BOOLEAN]
ScanPhases == {"Idle", "Captured", "Accepted"}

VARIABLES
    rootRevision,
    rootBits,
    usedRevisions,
    cells,
    frontierRevision,
    descriptorActive,
    descriptorRevision,
    descriptorPredecessor,
    descriptorAffected,
    descriptorExpected,
    descriptorTarget,
    scanPhase,
    scanRootRevision,
    scanExpectedBits,
    scanReadBits,
    badAcceptedScan,
    badEarlyFrontier

vars ==
    <<rootRevision, rootBits, usedRevisions, cells, frontierRevision,
      descriptorActive, descriptorRevision, descriptorPredecessor,
      descriptorAffected, descriptorExpected, descriptorTarget, scanPhase,
      scanRootRevision, scanExpectedBits, scanReadBits, badAcceptedScan,
      badEarlyFrontier>>

BitsOf(wordCells) == [word \in Words |-> wordCells[word].bit]

FreshRevision(revision) ==
    revision \in Revisions /\ revision \notin usedRevisions

TargetFor(affected) ==
    [word \in Words |->
       IF word \in affected THEN ~rootBits[word] ELSE rootBits[word]]

DescriptorComplete ==
    \A word \in descriptorAffected :
        cells[word] =
          [bit |-> descriptorTarget[word], tag |-> descriptorRevision]

TypeOK ==
    /\ rootRevision \in Revisions
    /\ rootBits \in Bits
    /\ usedRevisions \subseteq Revisions
    /\ rootRevision \in usedRevisions
    /\ cells \in Cells
    /\ frontierRevision \in Revisions
    /\ descriptorActive \in BOOLEAN
    /\ descriptorRevision \in Revisions \cup {NoRevision}
    /\ descriptorPredecessor \in Revisions \cup {NoRevision}
    /\ descriptorAffected \subseteq Words
    /\ descriptorExpected \in Cells
    /\ descriptorTarget \in Bits
    /\ scanPhase \in ScanPhases
    /\ scanRootRevision \in Revisions \cup {NoRevision}
    /\ scanExpectedBits \in Bits
    /\ scanReadBits \in Bits
    /\ badAcceptedScan \in BOOLEAN
    /\ badEarlyFrontier \in BOOLEAN

MaterializedMatchesFrontierRoot ==
    frontierRevision = rootRevision => BitsOf(cells) = rootBits

PublishedDescriptorIsExactSuccessor ==
    descriptorActive =>
        /\ descriptorRevision = rootRevision
        /\ descriptorPredecessor = frontierRevision

NoAcceptedTornScan == ~badAcceptedScan
NoEarlyFrontier == ~badEarlyFrontier

Init ==
    LET initialRevision == CHOOSE revision \in Revisions : TRUE
        initialBits == [word \in Words |-> FALSE]
    IN  /\ rootRevision = initialRevision
        /\ rootBits = initialBits
        /\ usedRevisions = {initialRevision}
        /\ cells =
             [word \in Words |-> [bit |-> FALSE, tag |-> initialRevision]]
        /\ frontierRevision = initialRevision
        /\ descriptorActive = FALSE
        /\ descriptorRevision = NoRevision
        /\ descriptorPredecessor = NoRevision
        /\ descriptorAffected = {}
        /\ descriptorExpected = cells
        /\ descriptorTarget = initialBits
        /\ scanPhase = "Idle"
        /\ scanRootRevision = NoRevision
        /\ scanExpectedBits = initialBits
        /\ scanReadBits = initialBits
        /\ badAcceptedScan = FALSE
        /\ badEarlyFrontier = FALSE

PublishTransition(affected, revision) ==
    /\ affected \subseteq Words
    /\ affected # {}
    /\ FreshRevision(revision)
    /\ ~descriptorActive
    /\ frontierRevision = rootRevision
    /\ rootRevision' = revision
    /\ rootBits' = TargetFor(affected)
    /\ usedRevisions' = usedRevisions \cup {revision}
    /\ descriptorActive' = TRUE
    /\ descriptorRevision' = revision
    /\ descriptorPredecessor' = rootRevision
    /\ descriptorAffected' = affected
    /\ descriptorExpected' = cells
    /\ descriptorTarget' = TargetFor(affected)
    /\ UNCHANGED <<cells, frontierRevision, scanPhase, scanRootRevision,
                    scanExpectedBits, scanReadBits, badAcceptedScan,
                    badEarlyFrontier>>

HelpWord(word) ==
    /\ descriptorActive
    /\ word \in descriptorAffected
    /\ cells[word] = descriptorExpected[word]
    /\ cells' =
         [cells EXCEPT
            ![word] = [bit |-> descriptorTarget[word],
                       tag |-> descriptorRevision]]
    /\ UNCHANGED <<rootRevision, rootBits, usedRevisions, frontierRevision,
                    descriptorActive, descriptorRevision,
                    descriptorPredecessor, descriptorAffected,
                    descriptorExpected, descriptorTarget, scanPhase,
                    scanRootRevision, scanExpectedBits, scanReadBits,
                    badAcceptedScan, badEarlyFrontier>>

AdvanceFrontier ==
    /\ descriptorActive
    /\ (DescriptorComplete \/ UnsafeEarlyFrontier)
    /\ frontierRevision' = descriptorRevision
    /\ descriptorActive' = FALSE
    /\ descriptorRevision' = NoRevision
    /\ descriptorPredecessor' = NoRevision
    /\ descriptorAffected' = {}
    /\ badEarlyFrontier' = (badEarlyFrontier \/ ~DescriptorComplete)
    /\ UNCHANGED <<rootRevision, rootBits, usedRevisions, cells,
                    descriptorExpected, descriptorTarget, scanPhase,
                    scanRootRevision, scanExpectedBits, scanReadBits,
                    badAcceptedScan>>

BeginScan ==
    /\ scanPhase = "Idle"
    /\ scanPhase' = "Captured"
    /\ scanRootRevision' = rootRevision
    /\ scanExpectedBits' = rootBits
    /\ UNCHANGED <<rootRevision, rootBits, usedRevisions, cells,
                    frontierRevision, descriptorActive, descriptorRevision,
                    descriptorPredecessor, descriptorAffected,
                    descriptorExpected, descriptorTarget, scanReadBits,
                    badAcceptedScan, badEarlyFrontier>>

AcceptScan ==
    /\ scanPhase = "Captured"
    /\ (UnsafeScanSkipsRevalidation
        \/ /\ rootRevision = scanRootRevision
           /\ frontierRevision = scanRootRevision)
    /\ scanPhase' = "Accepted"
    /\ scanReadBits' = BitsOf(cells)
    /\ badAcceptedScan' =
         (badAcceptedScan
          \/ BitsOf(cells) # scanExpectedBits
          \/ rootRevision # scanRootRevision
          \/ frontierRevision # scanRootRevision)
    /\ UNCHANGED <<rootRevision, rootBits, usedRevisions, cells,
                    frontierRevision, descriptorActive, descriptorRevision,
                    descriptorPredecessor, descriptorAffected,
                    descriptorExpected, descriptorTarget, scanRootRevision,
                    scanExpectedBits, badEarlyFrontier>>

ResetScan ==
    /\ scanPhase = "Accepted"
    /\ scanPhase' = "Idle"
    /\ scanRootRevision' = NoRevision
    /\ UNCHANGED <<rootRevision, rootBits, usedRevisions, cells,
                    frontierRevision, descriptorActive, descriptorRevision,
                    descriptorPredecessor, descriptorAffected,
                    descriptorExpected, descriptorTarget, scanExpectedBits,
                    scanReadBits, badAcceptedScan, badEarlyFrontier>>

Next ==
    \/ \E affected \in SUBSET Words, revision \in Revisions :
         PublishTransition(affected, revision)
    \/ \E word \in Words : HelpWord(word)
    \/ AdvanceFrontier
    \/ BeginScan
    \/ AcceptScan
    \/ ResetScan

Spec == Init /\ [][Next]_vars

(** Under weak fairness, a continuously enabled helper cannot be postponed
    forever. Every affected word is therefore materialized and the frontier
    eventually leaves a published descriptor. No particular publisher must
    resume; any thread may execute the enabled help actions. *)
FairSpec ==
    Spec
    /\ (\A word \in Words : WF_vars(HelpWord(word)))
    /\ WF_vars(AdvanceFrontier)

DescriptorEventuallyMaterialized ==
    descriptorActive ~> ~descriptorActive

=============================================================================
