--------------------------- MODULE OverlayTreeWitness ---------------------------
EXTENDS Naturals, TLC

(***************************************************************************
 * Finite revision/CAS abstraction for the sealed resident-tree witness.    *
 * Rocq supplies the unbounded graph and serializer-refinement proofs; this *
 * model checks witness lifecycle across publication and CAS interleavings. *
 ****************************************************************************)

CONSTANT UnsafeMode

Revisions == {"R0", "R1", "R2"}
Topologies == {"Tree", "Dag", "Cycle"}
NoWitness == "None"

VARIABLES revision, topology, witnessRevision, fastSerialize

vars == <<revision, topology, witnessRevision, fastSerialize>>

NextRevision ==
  CASE revision = "R0" -> "R1"
    [] revision = "R1" -> "R2"
    [] OTHER -> "R0"

Init ==
  /\ revision = "R0"
  /\ topology = "Tree"
  /\ witnessRevision = "R0"
  /\ fastSerialize = FALSE

(***************************************************************************
 * A validated fresh/rebuilt root may mint a witness.  A validated shared   *
 * DAG remains unwitnessed.  A cyclic decoder candidate has no publish      *
 * action at all: it is rejected before reaching this revision machine.     *
 ****************************************************************************)
LoadTree ==
  /\ revision' = NextRevision
  /\ topology' = "Tree"
  /\ witnessRevision' = NextRevision
  /\ fastSerialize' = FALSE

LoadDag ==
  /\ revision' = NextRevision
  /\ topology' = "Dag"
  /\ witnessRevision' = NoWitness
  /\ fastSerialize' = FALSE

(***************************************************************************
 * Insert/remove/value/bulk/merge/fault/evict candidates preserve a witness *
 * only when the exact predecessor revision was witnessed and the proven    *
 * structural transformer preserves its topology.  An unwitnessed DAG never *
 * upgrades through an incremental publication.                             *
 ****************************************************************************)
PublishPreservingWinner ==
  /\ topology \in {"Tree", "Dag"}
  /\ revision' = NextRevision
  /\ topology' = topology
  /\ witnessRevision' =
       IF topology = "Tree" /\ witnessRevision = revision
       THEN NextRevision
       ELSE NoWitness
  /\ fastSerialize' = FALSE

MetadataWinner == PublishPreservingWinner

(** A losing root CAS publishes neither the candidate nor its witness. *)
CasLoser == UNCHANGED vars

EnableFast ==
  /\ topology = "Tree"
  /\ witnessRevision = revision
  /\ fastSerialize' = TRUE
  /\ UNCHANGED <<revision, topology, witnessRevision>>

DisableFast ==
  /\ fastSerialize' = FALSE
  /\ UNCHANGED <<revision, topology, witnessRevision>>

(***************************************************************************
 * Deliberately unsafe controls.  Each is enabled by exactly one cfg file.   *
 ****************************************************************************)
UnsafeForgeDagWitness ==
  /\ UnsafeMode = "ForgeDag"
  /\ revision' = NextRevision
  /\ topology' = "Dag"
  /\ witnessRevision' = NextRevision
  /\ fastSerialize' = FALSE

UnsafeReuseStaleWitness ==
  /\ UnsafeMode = "StaleWitness"
  /\ topology = "Tree"
  /\ witnessRevision = revision
  /\ revision' = NextRevision
  /\ topology' = "Tree"
  /\ witnessRevision' = witnessRevision
  /\ fastSerialize' = FALSE

UnsafeFastWithoutWitness ==
  /\ UnsafeMode = "FastWithoutWitness"
  /\ witnessRevision = NoWitness
  /\ fastSerialize' = TRUE
  /\ UNCHANGED <<revision, topology, witnessRevision>>

UnsafeAdmitCycle ==
  /\ UnsafeMode = "AdmitCycle"
  /\ revision' = NextRevision
  /\ topology' = "Cycle"
  /\ witnessRevision' = NoWitness
  /\ fastSerialize' = FALSE

Next ==
  \/ LoadTree
  \/ LoadDag
  \/ PublishPreservingWinner
  \/ MetadataWinner
  \/ CasLoser
  \/ EnableFast
  \/ DisableFast
  \/ UnsafeForgeDagWitness
  \/ UnsafeReuseStaleWitness
  \/ UnsafeFastWithoutWitness
  \/ UnsafeAdmitCycle

Spec == Init /\ [][Next]_vars

TypeOK ==
  /\ revision \in Revisions
  /\ topology \in Topologies
  /\ witnessRevision \in (Revisions \cup {NoWitness})
  /\ fastSerialize \in BOOLEAN

WitnessNamesCurrentRevision ==
  witnessRevision # NoWitness => witnessRevision = revision

WitnessImpliesTree ==
  witnessRevision # NoWitness => topology = "Tree"

DagNeverAcquiresWitness ==
  topology = "Dag" => witnessRevision = NoWitness

CycleIsNeverPublished == topology # "Cycle"

FastSerializationRequiresCurrentTreeWitness ==
  fastSerialize =>
    topology = "Tree" /\ witnessRevision = revision

=============================================================================
