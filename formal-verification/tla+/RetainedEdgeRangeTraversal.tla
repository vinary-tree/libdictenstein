-------------------- MODULE RetainedEdgeRangeTraversal --------------------
(***************************************************************************)
(* Retained immutable edge-range traversal under concurrent publication.   *)
(*                                                                         *)
(* A reader captures one published revision, emits the first edge in the    *)
(* fused start observation, and advances a nonempty sibling-range token.    *)
(* Writers may publish and reclaim revisions concurrently, but reclamation  *)
(* is forbidden while any reader retains the revision. Output remains local *)
(* until a complete traversal commits it atomically.                        *)
(***************************************************************************)

EXTENDS Naturals, FiniteSets, TLC

CONSTANTS Readers, MaxRevision, MaxEdges

ReaderStatuses == {"Idle", "Traversing", "Complete", "Failed", "Committed"}
Revisions == 0..MaxRevision
EdgeIndexes == 0..MaxEdges

Degree(revision) == revision % (MaxEdges + 1)
Prefix(count) == IF count = 0 THEN {} ELSE 0..(count - 1)

VARIABLES
  currentRevision,
  allocatedRevisions,
  readerStatus,
  readerRevision,
  nextIndex,
  rangeEnd,
  localOutput,
  externalWriter,
  writerBefore

vars == <<currentRevision, allocatedRevisions, readerStatus, readerRevision,
          nextIndex, rangeEnd, localOutput, externalWriter, writerBefore>>

Init ==
  /\ currentRevision = 0
  /\ allocatedRevisions = {0}
  /\ readerStatus = [reader \in Readers |-> "Idle"]
  /\ readerRevision = [reader \in Readers |-> 0]
  /\ nextIndex = [reader \in Readers |-> 0]
  /\ rangeEnd = [reader \in Readers |-> 0]
  /\ localOutput = [reader \in Readers |-> {}]
  /\ externalWriter = [reader \in Readers |-> {}]
  /\ writerBefore = [reader \in Readers |-> {}]

StartSuccess(reader) ==
  /\ reader \in Readers
  /\ readerStatus[reader] = "Idle"
  /\ LET degree == Degree(currentRevision) IN
     /\ readerStatus' = [readerStatus EXCEPT
          ![reader] = IF degree <= 1 THEN "Complete" ELSE "Traversing"]
     /\ readerRevision' = [readerRevision EXCEPT ![reader] = currentRevision]
     /\ nextIndex' = [nextIndex EXCEPT
          ![reader] = IF degree = 0 THEN 0 ELSE 1]
     /\ rangeEnd' = [rangeEnd EXCEPT ![reader] = degree]
     /\ localOutput' = [localOutput EXCEPT
          ![reader] = IF degree = 0 THEN {} ELSE {0}]
     /\ writerBefore' = [writerBefore EXCEPT
          ![reader] = externalWriter[reader]]
  /\ UNCHANGED <<currentRevision, allocatedRevisions, externalWriter>>

ReserveFailure(reader) ==
  /\ reader \in Readers
  /\ readerStatus[reader] = "Idle"
  /\ Degree(currentRevision) >= 2
  /\ readerStatus' = [readerStatus EXCEPT ![reader] = "Failed"]
  /\ readerRevision' = [readerRevision EXCEPT ![reader] = currentRevision]
  /\ nextIndex' = [nextIndex EXCEPT ![reader] = 0]
  /\ rangeEnd' = [rangeEnd EXCEPT ![reader] = Degree(currentRevision)]
  /\ localOutput' = [localOutput EXCEPT ![reader] = {}]
  /\ writerBefore' = [writerBefore EXCEPT
       ![reader] = externalWriter[reader]]
  /\ UNCHANGED <<currentRevision, allocatedRevisions, externalWriter>>

StepSuccess(reader) ==
  /\ reader \in Readers
  /\ readerStatus[reader] = "Traversing"
  /\ nextIndex[reader] < rangeEnd[reader]
  /\ LET advanced == nextIndex[reader] + 1 IN
     /\ readerStatus' = [readerStatus EXCEPT
          ![reader] = IF advanced = rangeEnd[reader]
                      THEN "Complete" ELSE "Traversing"]
     /\ nextIndex' = [nextIndex EXCEPT ![reader] = advanced]
  /\ localOutput' = [localOutput EXCEPT
       ![reader] = @ \cup {nextIndex[reader]}]
  /\ UNCHANGED <<currentRevision, allocatedRevisions, readerRevision,
                  rangeEnd, externalWriter, writerBefore>>

StepFailure(reader) ==
  /\ reader \in Readers
  /\ readerStatus[reader] = "Traversing"
  /\ readerStatus' = [readerStatus EXCEPT ![reader] = "Failed"]
  /\ UNCHANGED <<currentRevision, allocatedRevisions, readerRevision,
                  nextIndex, rangeEnd, localOutput,
                  externalWriter, writerBefore>>

Commit(reader) ==
  /\ reader \in Readers
  /\ readerStatus[reader] = "Complete"
  /\ readerStatus' = [readerStatus EXCEPT ![reader] = "Committed"]
  /\ externalWriter' = [externalWriter EXCEPT
       ![reader] = localOutput[reader]]
  /\ UNCHANGED <<currentRevision, allocatedRevisions, readerRevision,
                  nextIndex, rangeEnd, localOutput, writerBefore>>

Release(reader) ==
  /\ reader \in Readers
  /\ readerStatus[reader] \in {"Failed", "Committed"}
  /\ readerStatus' = [readerStatus EXCEPT ![reader] = "Idle"]
  /\ nextIndex' = [nextIndex EXCEPT ![reader] = 0]
  /\ rangeEnd' = [rangeEnd EXCEPT ![reader] = 0]
  /\ localOutput' = [localOutput EXCEPT ![reader] = {}]
  /\ writerBefore' = [writerBefore EXCEPT
       ![reader] = externalWriter[reader]]
  /\ UNCHANGED <<currentRevision, allocatedRevisions, readerRevision,
                  externalWriter>>

Publish ==
  /\ currentRevision < MaxRevision
  /\ currentRevision' = currentRevision + 1
  /\ allocatedRevisions' = allocatedRevisions \cup {currentRevision + 1}
  /\ UNCHANGED <<readerStatus, readerRevision, nextIndex, rangeEnd,
                  localOutput, externalWriter, writerBefore>>

Retained(revision) ==
  \E reader \in Readers :
    /\ readerStatus[reader] # "Idle"
    /\ readerRevision[reader] = revision

Reclaim(revision) ==
  /\ revision \in allocatedRevisions
  /\ revision # currentRevision
  /\ ~Retained(revision)
  /\ allocatedRevisions' = allocatedRevisions \ {revision}
  /\ UNCHANGED <<currentRevision, readerStatus, readerRevision,
                  nextIndex, rangeEnd, localOutput,
                  externalWriter, writerBefore>>

Next ==
  \/ \E reader \in Readers : StartSuccess(reader)
  \/ \E reader \in Readers : ReserveFailure(reader)
  \/ \E reader \in Readers : StepSuccess(reader)
  \/ \E reader \in Readers : StepFailure(reader)
  \/ \E reader \in Readers : Commit(reader)
  \/ \E reader \in Readers : Release(reader)
  \/ Publish
  \/ \E revision \in Revisions : Reclaim(revision)

TypeInvariant ==
  /\ currentRevision \in Revisions
  /\ allocatedRevisions \subseteq Revisions
  /\ readerStatus \in [Readers -> ReaderStatuses]
  /\ readerRevision \in [Readers -> Revisions]
  /\ nextIndex \in [Readers -> EdgeIndexes]
  /\ rangeEnd \in [Readers -> EdgeIndexes]
  /\ localOutput \in [Readers -> SUBSET EdgeIndexes]
  /\ externalWriter \in [Readers -> SUBSET EdgeIndexes]
  /\ writerBefore \in [Readers -> SUBSET EdgeIndexes]

CurrentRevisionIsAllocated == currentRevision \in allocatedRevisions

RetainedReaderRevisionIsAllocated ==
  \A reader \in Readers :
    readerStatus[reader] # "Idle" =>
      readerRevision[reader] \in allocatedRevisions

RangeBounds ==
  \A reader \in Readers :
    readerStatus[reader] \in {"Traversing", "Complete"} =>
      /\ rangeEnd[reader] = Degree(readerRevision[reader])
      /\ nextIndex[reader] <= rangeEnd[reader]

TraversalIsExactPrefix ==
  \A reader \in Readers :
    readerStatus[reader] \in {"Traversing", "Complete"} =>
      localOutput[reader] = Prefix(nextIndex[reader])

CompleteTraversalReachedExactEnd ==
  \A reader \in Readers :
    readerStatus[reader] = "Complete" =>
      nextIndex[reader] = rangeEnd[reader]

NoPartialExternalPublication ==
  \A reader \in Readers :
    readerStatus[reader] \in {"Traversing", "Complete", "Failed"} =>
      externalWriter[reader] = writerBefore[reader]

CommittedWriterIsExact ==
  \A reader \in Readers :
    readerStatus[reader] = "Committed" =>
      externalWriter[reader] = Prefix(Degree(readerRevision[reader]))

Spec == Init /\ [][Next]_vars

(***************************************************************************)
(* Negative controls selected by dedicated TLC configurations.             *)
(***************************************************************************)

UnsafeReclaimRetained(revision) ==
  /\ revision \in allocatedRevisions
  /\ revision # currentRevision
  /\ Retained(revision)
  /\ allocatedRevisions' = allocatedRevisions \ {revision}
  /\ UNCHANGED <<currentRevision, readerStatus, readerRevision,
                  nextIndex, rangeEnd, localOutput,
                  externalWriter, writerBefore>>

UnsafePublishPartialStep(reader) ==
  /\ reader \in Readers
  /\ readerStatus[reader] = "Traversing"
  /\ nextIndex[reader] < rangeEnd[reader]
  /\ readerStatus' = readerStatus
  /\ readerRevision' = readerRevision
  /\ nextIndex' = nextIndex
  /\ rangeEnd' = rangeEnd
  /\ localOutput' = localOutput
  /\ externalWriter' = [externalWriter EXCEPT
       ![reader] = localOutput[reader] \cup {nextIndex[reader]}]
  /\ writerBefore' = writerBefore
  /\ UNCHANGED <<currentRevision, allocatedRevisions>>

UnsafeAdvancePastEnd(reader) ==
  /\ reader \in Readers
  /\ readerStatus[reader] = "Complete"
  /\ readerStatus' = [readerStatus EXCEPT ![reader] = "Traversing"]
  /\ nextIndex' = [nextIndex EXCEPT ![reader] = @ + 1]
  /\ UNCHANGED <<currentRevision, allocatedRevisions, readerRevision,
                  rangeEnd, localOutput, externalWriter, writerBefore>>

SpecUnsafeReclaim ==
  Init /\ [][Next \/
    (\E revision \in Revisions : UnsafeReclaimRetained(revision))]_vars

SpecUnsafePartialPublish ==
  Init /\ [][Next \/
    (\E reader \in Readers : UnsafePublishPartialStep(reader))]_vars

SpecUnsafeAdvancePastEnd ==
  Init /\ [][Next \/
    (\E reader \in Readers : UnsafeAdvancePastEnd(reader))]_vars

=============================================================================
